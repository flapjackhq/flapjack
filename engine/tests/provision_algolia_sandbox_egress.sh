#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2016

set -euo pipefail
umask 077

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VALIDATOR="$SCRIPT_DIR/validate_algolia_sandbox_cleanup_ledger.py"

usage() {
  echo "Usage: $0 --secret-file PATH --ledger PATH --stage6-receipt PATH --artifact-dir PATH --fjcloud-repo PATH [--input-contract-only|--prepare-retry-only]" >&2
}

read_required_json() {
  local path="$1"
  local expression="$2"
  local label="$3"
  local value=""
  value="$(jq -er "$expression | select(. != null and . != \"\")" "$path")" || {
    echo "Missing required $label in $path" >&2
    return 1
  }
  printf '%s\n' "$value"
}

ledger_update() {
  local filter="$1"
  local temporary=""
  shift
  temporary="$(mktemp "$(dirname "$LEDGER")/.stage6_egress_ledger.XXXXXX")"
  jq "$@" "$filter" "$LEDGER" > "$temporary"
  python3 "$VALIDATOR" "$temporary" --self-test >/dev/null
  mv "$temporary" "$LEDGER"
}

replace_ledger_token() {
  local resource_key="$1"
  local token="$2"
  local value="$3"
  ledger_update \
    '.stage6_egress.resources[$key] = $value
     | (.stage6_egress.cleanup_commands[] |= gsub($token; $value))
     | (.stage8_zero_residue_assertions[].command |= gsub($token; $value))' \
    --arg key "$resource_key" --arg token "$token" --arg value "$value"
}

aws_ignore_not_found() {
  "$@" >/dev/null 2>&1 || true
}

recover_created_resource_ids() {
  if [ -z "$INTERNET_GATEWAY_ID" ] && [ -s "${IGW_CREATE_RESPONSE:-}" ]; then
    INTERNET_GATEWAY_ID="$(jq -r '.InternetGateway.InternetGatewayId // empty' "$IGW_CREATE_RESPONSE")"
  fi
  if [ -z "$PUBLIC_SUBNET_ID" ] && [ -s "${SUBNET_CREATE_RESPONSE:-}" ]; then
    PUBLIC_SUBNET_ID="$(jq -r '.Subnet.SubnetId // empty' "$SUBNET_CREATE_RESPONSE")"
  fi
  if [ -z "$PUBLIC_ROUTE_TABLE_ID" ] && [ -s "${ROUTE_TABLE_CREATE_RESPONSE:-}" ]; then
    PUBLIC_ROUTE_TABLE_ID="$(jq -r '.RouteTable.RouteTableId // empty' "$ROUTE_TABLE_CREATE_RESPONSE")"
  fi
  if [ -z "$PUBLIC_ROUTE_ASSOCIATION_ID" ] && [ -s "${ROUTE_ASSOCIATION_RESPONSE:-}" ]; then
    PUBLIC_ROUTE_ASSOCIATION_ID="$(jq -r '.AssociationId // empty' "$ROUTE_ASSOCIATION_RESPONSE")"
  fi
  if [ -z "$EIP_ALLOCATION_ID" ] && [ -s "${EIP_CREATE_RESPONSE:-}" ]; then
    EIP_ALLOCATION_ID="$(jq -r '.AllocationId // empty' "$EIP_CREATE_RESPONSE")"
  fi
  if [ -z "$NAT_GATEWAY_ID" ] && [ -s "${NAT_CREATE_RESPONSE:-}" ]; then
    NAT_GATEWAY_ID="$(jq -r '.NatGateway.NatGatewayId // empty' "$NAT_CREATE_RESPONSE")"
  fi
}

cleanup_egress() {
  trap - EXIT INT TERM
  set +e
  recover_created_resource_ids
  if [ "$PRIVATE_ROUTE_CREATED" = true ]; then
    aws_ignore_not_found aws ec2 delete-route --region "$REGION" \
      --route-table-id "$PRIVATE_ROUTE_TABLE_ID" --destination-cidr-block 0.0.0.0/0
  fi
  if [ -n "$NAT_GATEWAY_ID" ]; then
    aws_ignore_not_found aws ec2 delete-nat-gateway --region "$REGION" \
      --nat-gateway-id "$NAT_GATEWAY_ID"
    aws ec2 wait nat-gateway-deleted --region "$REGION" \
      --nat-gateway-ids "$NAT_GATEWAY_ID" >/dev/null 2>&1 || true
  fi
  if [ -n "$EIP_ALLOCATION_ID" ]; then
    aws_ignore_not_found aws ec2 release-address --region "$REGION" \
      --allocation-id "$EIP_ALLOCATION_ID"
  fi
  if [ -n "$PUBLIC_ROUTE_ASSOCIATION_ID" ]; then
    aws_ignore_not_found aws ec2 disassociate-route-table --region "$REGION" \
      --association-id "$PUBLIC_ROUTE_ASSOCIATION_ID"
  fi
  if [ -n "$PUBLIC_ROUTE_TABLE_ID" ]; then
    aws_ignore_not_found aws ec2 delete-route --region "$REGION" \
      --route-table-id "$PUBLIC_ROUTE_TABLE_ID" --destination-cidr-block 0.0.0.0/0
    aws_ignore_not_found aws ec2 delete-route-table --region "$REGION" \
      --route-table-id "$PUBLIC_ROUTE_TABLE_ID"
  fi
  if [ -n "$PUBLIC_SUBNET_ID" ]; then
    aws_ignore_not_found aws ec2 delete-subnet --region "$REGION" --subnet-id "$PUBLIC_SUBNET_ID"
  fi
  if [ "$IGW_ATTACHED" = true ] && [ -n "$INTERNET_GATEWAY_ID" ]; then
    aws_ignore_not_found aws ec2 detach-internet-gateway --region "$REGION" \
      --internet-gateway-id "$INTERNET_GATEWAY_ID" --vpc-id "$VPC_ID"
  fi
  if [ -n "$INTERNET_GATEWAY_ID" ]; then
    aws_ignore_not_found aws ec2 delete-internet-gateway --region "$REGION" \
      --internet-gateway-id "$INTERNET_GATEWAY_ID"
  fi
  ledger_update '.stage6_egress.status = "cleanup_attempted_after_failure"' || true
}

wait_for_ssm_command() {
  local command_id="$1"
  local status=""
  local _attempt=0
  for _attempt in $(seq 1 60); do
    status="$(aws ssm get-command-invocation --region "$REGION" --command-id "$command_id" \
      --instance-id "$INSTANCE_ID" --query Status --output text 2>/dev/null || true)"
    case "$status" in
      Success) return 0 ;;
      Failed|Cancelled|TimedOut) return 1 ;;
    esac
    sleep 2
  done
  return 1
}

prepare_retry_ledger() {
  [ "$(jq -r '.stage6_egress.status' "$LEDGER")" = cleanup_attempted_after_failure ] || return 0

  local prior_tag=""
  local cleanup_proved_utc=""
  prior_tag="$(jq -er '.stage6_egress.run_tag' "$LEDGER")"
  [ "$(aws ec2 describe-internet-gateways --region "$REGION" --filters "Name=tag:RunId,Values=$prior_tag" \
    --query 'length(InternetGateways)' --output text)" = 0 ]
  [ "$(aws ec2 describe-subnets --region "$REGION" --filters "Name=tag:RunId,Values=$prior_tag" \
    --query 'length(Subnets)' --output text)" = 0 ]
  [ "$(aws ec2 describe-route-tables --region "$REGION" --filters "Name=tag:RunId,Values=$prior_tag" \
    --query 'length(RouteTables)' --output text)" = 0 ]
  [ "$(aws ec2 describe-nat-gateways --region "$REGION" --filter "Name=tag:RunId,Values=$prior_tag" \
    --query 'length(NatGateways[?State!=`deleted`])' --output text)" = 0 ]
  [ "$(aws ec2 describe-addresses --region "$REGION" --filters "Name=tag:RunId,Values=$prior_tag" \
    --query 'length(Addresses)' --output text)" = 0 ]
  [ "$(aws ec2 describe-route-tables --region "$REGION" --route-table-ids "$PRIVATE_ROUTE_TABLE_ID" \
    --query 'length(RouteTables[0].Routes[?DestinationCidrBlock==`0.0.0.0/0`])' --output text)" = 0 ]

  cleanup_proved_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  ledger_update \
    'def restore($old; $token):
       if ($old == null or $old == "") then . else gsub($old; $token) end;
     .stage6_egress.attempts = ((.stage6_egress.attempts // []) + [{
       run_tag: .stage6_egress.run_tag,
       resources: .stage6_egress.resources,
       outcome: "cleaned_after_failed_egress_attempt",
       cleanup_proved_utc: $proved,
       remaining_ids: []
     }])
     | (.stage6_egress.cleanup_commands[] |= (
         restore($igw; "{internet_gateway_id}")
         | restore($subnet; "{public_subnet_id}")
         | restore($route_table; "{public_route_table_id}")
         | restore($association; "{public_route_table_association_id}")
         | restore($eip; "{eip_allocation_id}")
         | restore($nat; "{nat_gateway_id}")
         | restore($enis; "{nat_gateway_eni_ids}")))
     | (.stage8_zero_residue_assertions[].command |= (
         restore($prior_tag; "{egress_run_tag}")
         | restore($igw; "{internet_gateway_id}")
         | restore($subnet; "{public_subnet_id}")
         | restore($route_table; "{public_route_table_id}")
         | restore($association; "{public_route_table_association_id}")
         | restore($eip; "{eip_allocation_id}")
         | restore($nat; "{nat_gateway_id}")
         | restore($enis; "{nat_gateway_eni_ids}")))
     | .stage6_egress.resources |= . + {
         internet_gateway_id: null,
         internet_gateway_attachment_vpc_id: null,
         public_subnet_id: null,
         public_route_table_id: null,
         public_route_table_association_id: null,
         nat_gateway_id: null,
         nat_gateway_eni_ids: [],
         eip_allocation_id: null,
         eip_association_id: null,
         additional_eni_ids: []
       }
     | .stage6_egress.run_tag = null
     | .stage6_egress.created_at_utc = null
     | .stage6_egress.status = "ledger_armed_retry_after_proven_cleanup"' \
    --arg proved "$cleanup_proved_utc" \
    --arg prior_tag "$prior_tag" \
    --arg igw "$(jq -r '.stage6_egress.resources.internet_gateway_id // ""' "$LEDGER")" \
    --arg subnet "$(jq -r '.stage6_egress.resources.public_subnet_id // ""' "$LEDGER")" \
    --arg route_table "$(jq -r '.stage6_egress.resources.public_route_table_id // ""' "$LEDGER")" \
    --arg association "$(jq -r '.stage6_egress.resources.public_route_table_association_id // ""' "$LEDGER")" \
    --arg eip "$(jq -r '.stage6_egress.resources.eip_allocation_id // ""' "$LEDGER")" \
    --arg nat "$(jq -r '.stage6_egress.resources.nat_gateway_id // ""' "$LEDGER")" \
    --arg enis "$(jq -r '.stage6_egress.resources.nat_gateway_eni_ids | join(" ")' "$LEDGER")"
}

SECRET_FILE=""
LEDGER=""
STAGE6_RECEIPT=""
ARTIFACT_DIR=""
FJCLOUD_REPO=""
INPUT_CONTRACT_ONLY=false
PREPARE_RETRY_ONLY=false
while [ "$#" -gt 0 ]; do
  case "$1" in
    --secret-file) SECRET_FILE="${2:-}"; shift 2 ;;
    --ledger) LEDGER="${2:-}"; shift 2 ;;
    --stage6-receipt) STAGE6_RECEIPT="${2:-}"; shift 2 ;;
    --artifact-dir) ARTIFACT_DIR="${2:-}"; shift 2 ;;
    --fjcloud-repo) FJCLOUD_REPO="${2:-}"; shift 2 ;;
    --input-contract-only) INPUT_CONTRACT_ONLY=true; shift ;;
    --prepare-retry-only) PREPARE_RETRY_ONLY=true; shift ;;
    *) usage; exit 2 ;;
  esac
done
[ -n "$SECRET_FILE" ] && [ -n "$LEDGER" ] && [ -n "$STAGE6_RECEIPT" ] \
  && [ -n "$ARTIFACT_DIR" ] && [ -n "$FJCLOUD_REPO" ] || {
  usage
  exit 2
}

mkdir -p "$ARTIFACT_DIR"
chmod 700 "$ARTIFACT_DIR"
LOG="$ARTIFACT_DIR/provision.log"
ERR_LOG="$ARTIFACT_DIR/provision.err.log"
exec > >(tee "$LOG") 2> >(tee "$ERR_LOG" >&2)

python3 "$VALIDATOR" "$LEDGER" --self-test
jq -e '.status == "provisioned_running_verified"' "$STAGE6_RECEIPT" >/dev/null

REGION="$(read_required_json "$STAGE6_RECEIPT" '.frozen_inputs.REGION' frozen_inputs.REGION)"
VPC_ID="$(read_required_json "$LEDGER" '.resources.vpc_id' resources.vpc_id)"
PRIVATE_SUBNET_ID="$(read_required_json "$STAGE6_RECEIPT" '.frozen_inputs.SUBNET_ID' frozen_inputs.SUBNET_ID)"
SECURITY_GROUP_ID="$(read_required_json "$STAGE6_RECEIPT" '.frozen_inputs.SECURITY_GROUP_ID' frozen_inputs.SECURITY_GROUP_ID)"
EXPECTED_AMI="$(read_required_json "$STAGE6_RECEIPT" '.frozen_inputs.AMI_ID' frozen_inputs.AMI_ID)"
EXPECTED_PROFILE="$(read_required_json "$STAGE6_RECEIPT" '.frozen_inputs.INSTANCE_PROFILE_NAME' frozen_inputs.INSTANCE_PROFILE_NAME)"
ROOT_VOLUME_ID="$(read_required_json "$STAGE6_RECEIPT" '.resource_ids.root_volume_id' resource_ids.root_volume_id)"
PRIMARY_ENI_ID="$(read_required_json "$STAGE6_RECEIPT" '.resource_ids.primary_eni_id' resource_ids.primary_eni_id)"
INSTANCE_RUN_TAG="$(read_required_json "$STAGE6_RECEIPT" '.run_tag' run_tag)"
EXPECTED_ACCOUNT="$(read_required_json "$STAGE6_RECEIPT" '.identity_label.account_id' identity_label.account_id)"
PRIVATE_ROUTE_TABLE_ID="$(read_required_json "$LEDGER" '.stage6_egress.resources.private_route_table_id' stage6_egress.resources.private_route_table_id)"
if [ "$INPUT_CONTRACT_ONLY" = true ]; then
  echo "ACCEPT: Stage 5 ledger and Stage 6 receipt input contract"
  exit 0
fi

unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN AWS_PROFILE AWS_DEFAULT_PROFILE
unset AWS_REGION AWS_DEFAULT_REGION
export AWS_EC2_METADATA_DISABLED=true AWS_PAGER=""
source "$SCRIPT_DIR/common/load_named_secrets.sh"
load_named_secrets "$SECRET_FILE" AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_DEFAULT_REGION
PUBLIC_SUBNET_CIDR="10.253.51.16/28"
START_UTC="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
EGRESS_RUN_TAG="algolia-migration-egress-stage6-$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 8)"

IDENTITY_JSON="$ARTIFACT_DIR/aws_identity.json"
aws sts get-caller-identity > "$IDENTITY_JSON"
jq -e --arg account "$EXPECTED_ACCOUNT" '.Account == $account' "$IDENTITY_JSON" >/dev/null
ARN_LABEL="$(jq -r '.Arn | split("/")[-1]' "$IDENTITY_JSON")"

INSTANCE_ID="$(aws ec2 describe-instances --region "$REGION" \
  --filters "Name=tag:Purpose,Values=$INSTANCE_RUN_TAG" "Name=instance-state-name,Values=pending,running,stopping,stopped" \
  --query 'Reservations[].Instances[].InstanceId' --output text)"
[ -n "$INSTANCE_ID" ] && [ "${INSTANCE_ID//$'\t'/}" = "$INSTANCE_ID" ]
aws ec2 wait instance-running --region "$REGION" --instance-ids "$INSTANCE_ID"
aws ec2 wait instance-status-ok --region "$REGION" --instance-ids "$INSTANCE_ID"

existing_subnets="$(aws ec2 describe-subnets --region "$REGION" --filters "Name=vpc-id,Values=$VPC_ID" \
  --query 'Subnets[].CidrBlock' --output text | tr '\t' ' ')"
[[ " $existing_subnets " != *" $PUBLIC_SUBNET_CIDR "* ]]
[ "$(aws ec2 describe-internet-gateways --region "$REGION" --filters "Name=attachment.vpc-id,Values=$VPC_ID" \
  --query 'length(InternetGateways)' --output text)" = 0 ]
[ "$(aws ec2 describe-nat-gateways --region "$REGION" --filter "Name=vpc-id,Values=$VPC_ID" \
  --query 'length(NatGateways[?State!=`deleted`])' --output text)" = 0 ]
prepare_retry_ledger
if [ "$PREPARE_RETRY_ONLY" = true ]; then
  [ "$(jq -r '.stage6_egress.status' "$LEDGER")" = ledger_armed_retry_after_proven_cleanup ]
  echo "ACCEPT: failed egress attempt has zero residue and ledger is re-armed"
  exit 0
fi

INTERNET_GATEWAY_ID=""
PUBLIC_SUBNET_ID=""
PUBLIC_ROUTE_TABLE_ID=""
PUBLIC_ROUTE_ASSOCIATION_ID=""
EIP_ALLOCATION_ID=""
NAT_GATEWAY_ID=""
NAT_GATEWAY_ENI_IDS=""
EIP_ASSOCIATION_ID=""
IGW_ATTACHED=false
PRIVATE_ROUTE_CREATED=false
trap cleanup_egress EXIT INT TERM

ledger_update \
  '.stage6_egress.run_tag = $tag
   | .stage6_egress.created_at_utc = $start
   | .stage6_egress.flapjack_sha = $flapjack_sha
   | .stage6_egress.fjcloud_sha = $fjcloud_sha
   | .stage6_egress.status = "provisioning"
   | (.stage8_zero_residue_assertions[].command |= gsub("\\{egress_run_tag\\}"; $tag))' \
  --arg tag "$EGRESS_RUN_TAG" --arg start "$START_UTC" \
  --arg flapjack_sha "$(git -C "$SCRIPT_DIR/../.." rev-parse HEAD)" \
  --arg fjcloud_sha "$(git -C "$FJCLOUD_REPO" rev-parse HEAD)"

TAGS="Key=Environment,Value=sandbox Key=Purpose,Value=algolia-migration-egress Key=RunId,Value=$EGRESS_RUN_TAG Key=ManagedBy,Value=codex-stage6-egress Key=RetainUntil,Value=Stage8 Key=Stage,Value=6"

EIP_CREATE_RESPONSE="$ARTIFACT_DIR/allocate_eip.json"
aws ec2 allocate-address --region "$REGION" --domain vpc \
  --tag-specifications "ResourceType=elastic-ip,Tags=[$(sed 's/ /},{/g; s/^/{/; s/$/}/' <<< "$TAGS")]" \
  > "$EIP_CREATE_RESPONSE"
EIP_ALLOCATION_ID="$(jq -er '.AllocationId' "$EIP_CREATE_RESPONSE")"
replace_ledger_token eip_allocation_id '\{eip_allocation_id\}' "$EIP_ALLOCATION_ID"

IGW_CREATE_RESPONSE="$ARTIFACT_DIR/create_internet_gateway.json"
aws ec2 create-internet-gateway --region "$REGION" \
  --tag-specifications "ResourceType=internet-gateway,Tags=[$(sed 's/ /},{/g; s/^/{/; s/$/}/' <<< "$TAGS")]" \
  > "$IGW_CREATE_RESPONSE"
INTERNET_GATEWAY_ID="$(jq -er '.InternetGateway.InternetGatewayId' "$IGW_CREATE_RESPONSE")"
replace_ledger_token internet_gateway_id '\{internet_gateway_id\}' "$INTERNET_GATEWAY_ID"
aws ec2 attach-internet-gateway --region "$REGION" --internet-gateway-id "$INTERNET_GATEWAY_ID" --vpc-id "$VPC_ID"
IGW_ATTACHED=true
ledger_update '.stage6_egress.resources.internet_gateway_attachment_vpc_id = $vpc' --arg vpc "$VPC_ID"

SUBNET_CREATE_RESPONSE="$ARTIFACT_DIR/create_public_subnet.json"
aws ec2 create-subnet --region "$REGION" --vpc-id "$VPC_ID" \
  --cidr-block "$PUBLIC_SUBNET_CIDR" --availability-zone us-east-1a \
  --tag-specifications "ResourceType=subnet,Tags=[$(sed 's/ /},{/g; s/^/{/; s/$/}/' <<< "$TAGS")]" \
  > "$SUBNET_CREATE_RESPONSE"
PUBLIC_SUBNET_ID="$(jq -er '.Subnet.SubnetId' "$SUBNET_CREATE_RESPONSE")"
replace_ledger_token public_subnet_id '\{public_subnet_id\}' "$PUBLIC_SUBNET_ID"

ROUTE_TABLE_CREATE_RESPONSE="$ARTIFACT_DIR/create_public_route_table.json"
aws ec2 create-route-table --region "$REGION" --vpc-id "$VPC_ID" \
  --tag-specifications "ResourceType=route-table,Tags=[$(sed 's/ /},{/g; s/^/{/; s/$/}/' <<< "$TAGS")]" \
  > "$ROUTE_TABLE_CREATE_RESPONSE"
PUBLIC_ROUTE_TABLE_ID="$(jq -er '.RouteTable.RouteTableId' "$ROUTE_TABLE_CREATE_RESPONSE")"
replace_ledger_token public_route_table_id '\{public_route_table_id\}' "$PUBLIC_ROUTE_TABLE_ID"
aws ec2 create-route --region "$REGION" --route-table-id "$PUBLIC_ROUTE_TABLE_ID" \
  --destination-cidr-block 0.0.0.0/0 --gateway-id "$INTERNET_GATEWAY_ID" \
  > "$ARTIFACT_DIR/create_public_default_route.json"
ROUTE_ASSOCIATION_RESPONSE="$ARTIFACT_DIR/associate_public_route_table.json"
aws ec2 associate-route-table --region "$REGION" \
  --route-table-id "$PUBLIC_ROUTE_TABLE_ID" --subnet-id "$PUBLIC_SUBNET_ID" \
  > "$ROUTE_ASSOCIATION_RESPONSE"
PUBLIC_ROUTE_ASSOCIATION_ID="$(jq -er '.AssociationId' "$ROUTE_ASSOCIATION_RESPONSE")"
replace_ledger_token public_route_table_association_id '\{public_route_table_association_id\}' \
  "$PUBLIC_ROUTE_ASSOCIATION_ID"

NAT_CREATE_RESPONSE="$ARTIFACT_DIR/create_nat_gateway.json"
aws ec2 create-nat-gateway --region "$REGION" --subnet-id "$PUBLIC_SUBNET_ID" \
  --allocation-id "$EIP_ALLOCATION_ID" \
  --tag-specifications "ResourceType=natgateway,Tags=[$(sed 's/ /},{/g; s/^/{/; s/$/}/' <<< "$TAGS")]" \
  > "$NAT_CREATE_RESPONSE"
NAT_GATEWAY_ID="$(jq -er '.NatGateway.NatGatewayId' "$NAT_CREATE_RESPONSE")"
replace_ledger_token nat_gateway_id '\{nat_gateway_id\}' "$NAT_GATEWAY_ID"
aws ec2 wait nat-gateway-available --region "$REGION" --nat-gateway-ids "$NAT_GATEWAY_ID"

NAT_GATEWAY_ENI_IDS="$(aws ec2 describe-nat-gateways --region "$REGION" --nat-gateway-ids "$NAT_GATEWAY_ID" \
  --query 'NatGateways[0].NatGatewayAddresses[].NetworkInterfaceId' --output text | tr '\t' ' ')"
EIP_ASSOCIATION_ID="$(aws ec2 describe-nat-gateways --region "$REGION" --nat-gateway-ids "$NAT_GATEWAY_ID" \
  --query 'NatGateways[0].NatGatewayAddresses[0].AssociationId' --output text)"
ledger_update \
  '.stage6_egress.resources.nat_gateway_eni_ids = ($enis | split(" "))
   | .stage6_egress.resources.additional_eni_ids = ($enis | split(" "))
   | .stage6_egress.resources.eip_association_id = $association
   | (.stage6_egress.cleanup_commands[] |= gsub("\\{nat_gateway_eni_ids\\}"; $enis))
   | (.stage8_zero_residue_assertions[].command |= gsub("\\{nat_gateway_eni_ids\\}"; $enis))' \
  --arg enis "$NAT_GATEWAY_ENI_IDS" --arg association "$EIP_ASSOCIATION_ID"

aws ec2 create-route --region "$REGION" --route-table-id "$PRIVATE_ROUTE_TABLE_ID" \
  --destination-cidr-block 0.0.0.0/0 --nat-gateway-id "$NAT_GATEWAY_ID" \
  > "$ARTIFACT_DIR/create_private_default_route.json"
PRIVATE_ROUTE_CREATED=true
ledger_update '.stage6_egress.status = "provisioned_pending_probe"'
python3 "$VALIDATOR" "$LEDGER" --self-test --require-populated

SSM_STATUS="$(aws ssm describe-instance-information --region "$REGION" \
  --filters "Key=InstanceIds,Values=$INSTANCE_ID" --query 'InstanceInformationList[0].PingStatus' --output text)"
[ "$SSM_STATUS" = Online ]
REMOTE_PROBE='set -eu
printf "UTC=%s\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf "ARCH=%s\n" "$(uname -m)"
all_success=true
algolia_success=false
probe_endpoint() {
  label=$1
  host=$2
  url=$3
  ip=$(getent ahostsv4 "$host" | awk "NR==1 {print \$1}")
  [ -n "$ip" ]
  printf "%s_DNS_IPV4=%s\n" "$label" "$ip"
  success=false
  for attempt in 1 2 3 4 5 6; do
    result=$(curl -sS -o /dev/null --connect-timeout 5 --max-time 15 -w "code=%{http_code} remote=%{remote_ip} tls=%{time_appconnect} exit=%{exitcode}" "$url") && {
      success=true
      printf "%s_attempt=%s\n" "$label" "$attempt"
      printf "%s_%s\n" "$label" "$result"
      break
    }
    printf "%s_attempt=%s result=%s\n" "$label" "$attempt" "$result"
    sleep 10
  done
  [ "$success" = true ]
}
if probe_endpoint ALGOLIA_PRIMARY latency-dsn.algolia.net https://latency-dsn.algolia.net/1/indexes; then
  algolia_success=true
fi
for spec in \
  "ALGOLIA_FALLBACK_1 latency-1.algolianet.com https://latency-1.algolianet.com/1/indexes" \
  "ALGOLIA_FALLBACK_2 latency-2.algolianet.com https://latency-2.algolianet.com/1/indexes" \
  "ALGOLIA_FALLBACK_3 latency-3.algolianet.com https://latency-3.algolianet.com/1/indexes"; do
  set -- $spec
  if probe_endpoint "$1" "$2" "$3"; then
    algolia_success=true
  fi
done
if [ "$algolia_success" != true ]; then
  all_success=false
fi
if ! probe_endpoint STS sts.amazonaws.com https://sts.amazonaws.com/; then
  all_success=false
fi
[ "$all_success" = true ]'
PROBE_PARAMS="$ARTIFACT_DIR/ssm_probe_parameters.json"
jq -n --arg command "$REMOTE_PROBE" '{commands:[$command]}' > "$PROBE_PARAMS"
COMMAND_ID="$(aws ssm send-command --region "$REGION" --instance-ids "$INSTANCE_ID" \
  --document-name AWS-RunShellScript --parameters "file://$PROBE_PARAMS" \
  --query Command.CommandId --output text)"
printf '%s\n' "$COMMAND_ID" > "$ARTIFACT_DIR/ssm_probe_command_id.txt"
PROBE_INVOCATION="$ARTIFACT_DIR/ssm_probe_invocation.json"
if ! wait_for_ssm_command "$COMMAND_ID"; then
  aws ssm get-command-invocation --region "$REGION" --command-id "$COMMAND_ID" \
    --instance-id "$INSTANCE_ID" > "$PROBE_INVOCATION" || true
  false
fi
aws ssm get-command-invocation --region "$REGION" --command-id "$COMMAND_ID" \
  --instance-id "$INSTANCE_ID" > "$PROBE_INVOCATION"
PROBE_STDOUT="$(jq -r '.StandardOutputContent' "$PROBE_INVOCATION")"
grep -Eq '^ARCH=aarch64$' <<< "$PROBE_STDOUT"
grep -Eq '^ALGOLIA_PRIMARY_DNS_IPV4=[0-9]' <<< "$PROBE_STDOUT"
grep -Eq '^ALGOLIA_FALLBACK_[123]_attempt=[1-6]$' <<< "$PROBE_STDOUT"
grep -Eq '^ALGOLIA_FALLBACK_[123]_code=[1-9][0-9][0-9] remote=[^ ]+ tls=0\.[0-9]*[1-9][0-9]* exit=0$' <<< "$PROBE_STDOUT"
grep -Eq '^STS_DNS_IPV4=[0-9]' <<< "$PROBE_STDOUT"
grep -Eq '^STS_attempt=[1-6]$' <<< "$PROBE_STDOUT"
grep -Eq '^STS_code=[1-9][0-9][0-9] remote=[^ ]+ tls=0\.[0-9]*[1-9][0-9]* exit=0$' <<< "$PROBE_STDOUT"

INSTANCE_READBACK="$ARTIFACT_DIR/instance_readback.json"
aws ec2 describe-instances --region "$REGION" --instance-ids "$INSTANCE_ID" > "$INSTANCE_READBACK"
jq -e --arg subnet "$PRIVATE_SUBNET_ID" --arg sg "$SECURITY_GROUP_ID" --arg ami "$EXPECTED_AMI" \
  --arg profile "$EXPECTED_PROFILE" --arg eni "$PRIMARY_ENI_ID" --arg volume "$ROOT_VOLUME_ID" \
  '.Reservations[0].Instances[0]
   | .State.Name == "running" and .InstanceType == "t4g.small" and .SubnetId == $subnet
   and .SecurityGroups[0].GroupId == $sg and .ImageId == $ami and .PublicIpAddress == null
   and (.IamInstanceProfile.Arn | endswith("/" + $profile))
   and .MetadataOptions.HttpTokens == "required" and .MetadataOptions.HttpEndpoint == "enabled"
   and .NetworkInterfaces[0].NetworkInterfaceId == $eni and .NetworkInterfaces[0].Association == null
   and (.BlockDeviceMappings | length) == 1 and .BlockDeviceMappings[0].Ebs.VolumeId == $volume
   and .BlockDeviceMappings[0].Ebs.DeleteOnTermination == true' "$INSTANCE_READBACK" >/dev/null

VOLUME_READBACK="$ARTIFACT_DIR/volume_readback.json"
aws ec2 describe-volumes --region "$REGION" --volume-ids "$ROOT_VOLUME_ID" > "$VOLUME_READBACK"
jq -e '.Volumes[0] | .Encrypted == true and .Size == 40 and .VolumeType == "gp3"' \
  "$VOLUME_READBACK" >/dev/null

NETWORK_READBACK="$ARTIFACT_DIR/network_readback.json"
jq -n \
  --argjson igw "$(aws ec2 describe-internet-gateways --region "$REGION" --internet-gateway-ids "$INTERNET_GATEWAY_ID")" \
  --argjson subnet "$(aws ec2 describe-subnets --region "$REGION" --subnet-ids "$PUBLIC_SUBNET_ID")" \
  --argjson public_routes "$(aws ec2 describe-route-tables --region "$REGION" --route-table-ids "$PUBLIC_ROUTE_TABLE_ID")" \
  --argjson private_routes "$(aws ec2 describe-route-tables --region "$REGION" --route-table-ids "$PRIVATE_ROUTE_TABLE_ID")" \
  --argjson nat "$(aws ec2 describe-nat-gateways --region "$REGION" --nat-gateway-ids "$NAT_GATEWAY_ID")" \
  --argjson eip "$(aws ec2 describe-addresses --region "$REGION" --allocation-ids "$EIP_ALLOCATION_ID")" \
  --argjson endpoints "$(aws ec2 describe-vpc-endpoints --region "$REGION" --filters "Name=vpc-id,Values=$VPC_ID")" \
  '{internet_gateway:$igw,public_subnet:$subnet,public_route_table:$public_routes,
    private_route_table:$private_routes,nat_gateway:$nat,eip:$eip,endpoints:$endpoints}' > "$NETWORK_READBACK"
jq -e --arg vpc "$VPC_ID" --arg public_subnet "$PUBLIC_SUBNET_ID" --arg igw "$INTERNET_GATEWAY_ID" \
  --arg nat "$NAT_GATEWAY_ID" --arg eip_assoc "$EIP_ASSOCIATION_ID" --arg run_tag "$EGRESS_RUN_TAG" \
  '.internet_gateway.InternetGateways[0].Attachments[0].VpcId == $vpc
   and (.internet_gateway.InternetGateways[0].Tags | any(.Key == "RunId" and .Value == $run_tag))
   and .public_subnet.Subnets[0].VpcId == $vpc
   and (.public_subnet.Subnets[0].Tags | any(.Key == "RunId" and .Value == $run_tag))
   and (.public_route_table.RouteTables[0].Associations | any(.SubnetId == $public_subnet and .AssociationState.State == "associated"))
   and (.public_route_table.RouteTables[0].Tags | any(.Key == "RunId" and .Value == $run_tag))
   and (.public_route_table.RouteTables[0].Routes | any(.DestinationCidrBlock == "0.0.0.0/0" and .GatewayId == $igw and .State == "active"))
   and (.private_route_table.RouteTables[0].Routes | any(.DestinationCidrBlock == "0.0.0.0/0" and .NatGatewayId == $nat and .State == "active"))
   and .nat_gateway.NatGateways[0].State == "available"
   and (.nat_gateway.NatGateways[0].Tags | any(.Key == "RunId" and .Value == $run_tag))
   and .eip.Addresses[0].AssociationId == $eip_assoc
   and (.eip.Addresses[0].Tags | any(.Key == "RunId" and .Value == $run_tag))
   and ([.endpoints.VpcEndpoints[] | select(.State == "available")] | length) == 3' "$NETWORK_READBACK" >/dev/null

END_UTC="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
ledger_update '.stage6_egress.status = "provisioned_verified_for_stage7"'
python3 "$VALIDATOR" "$LEDGER" --self-test --require-populated
LEDGER_DIGEST="$(shasum -a 256 "$LEDGER" | awk '{print $1}')"
FLAPJACK_SHA="$(git -C "$SCRIPT_DIR/../.." rev-parse HEAD)"
FJCLOUD_SHA="$(git -C "$FJCLOUD_REPO" rev-parse HEAD)"
RECEIPT="$ARTIFACT_DIR/s158_build_stage6_egress_recovery_receipt.json"
jq -n \
  --arg start "$START_UTC" --arg end "$END_UTC" --arg account "$EXPECTED_ACCOUNT" \
  --arg arn_label "$ARN_LABEL" --arg flapjack_sha "$FLAPJACK_SHA" --arg fjcloud_sha "$FJCLOUD_SHA" \
  --arg run_tag "$EGRESS_RUN_TAG" --arg instance "$INSTANCE_ID" --arg vpc "$VPC_ID" \
  --arg private_subnet "$PRIVATE_SUBNET_ID" --arg public_subnet "$PUBLIC_SUBNET_ID" \
  --arg igw "$INTERNET_GATEWAY_ID" --arg public_rt "$PUBLIC_ROUTE_TABLE_ID" \
  --arg public_assoc "$PUBLIC_ROUTE_ASSOCIATION_ID" --arg private_rt "$PRIVATE_ROUTE_TABLE_ID" \
  --arg nat "$NAT_GATEWAY_ID" --arg eip "$EIP_ALLOCATION_ID" --arg eip_assoc "$EIP_ASSOCIATION_ID" \
  --arg enis "$NAT_GATEWAY_ENI_IDS" --arg command_id "$COMMAND_ID" --arg probe "$PROBE_STDOUT" \
  --arg ledger "$LEDGER" --arg ledger_digest "$LEDGER_DIGEST" --arg log "$LOG" --arg err_log "$ERR_LOG" \
  '{stage:6,status:"outbound_https_restored",start_utc:$start,end_utc:$end,
    identity_label:{account_id:$account,arn_label:$arn_label},flapjack_sha:$flapjack_sha,fjcloud_sha:$fjcloud_sha,
    topology:{run_tag:$run_tag,instance_id:$instance,vpc_id:$vpc,private_subnet_id:$private_subnet,
      public_subnet_id:$public_subnet,internet_gateway_id:$igw,public_route_table_id:$public_rt,
      public_route_table_association_id:$public_assoc,private_route_table_id:$private_rt,nat_gateway_id:$nat,
      eip_allocation_id:$eip,eip_association_id:$eip_assoc,additional_eni_ids:($enis|split(" "))},
    frozen_contract:{instance_type:"t4g.small",public_ip:null,imds_tokens:"required",administration:"SSM only"},
    cost_projection:{max_runtime_hours:8,base_cost_usd:0.40,nat_processing_usd_per_gb:0.045,
      transfer_cost:"standard AWS data-transfer charges additionally apply",teardown_owner:"Stage 8"},
    credential_free_probe:{command_id:$command_id,stdout:$probe},canonical_cleanup_ledger:$ledger,
    canonical_cleanup_ledger_sha256:$ledger_digest,
    readbacks:{instance:"instance_readback.json",volume:"volume_readback.json",network:"network_readback.json"},
    logs:{stdout:$log,stderr:$err_log}}' > "$RECEIPT"

jq -e '.status == "outbound_https_restored"' "$RECEIPT" >/dev/null
trap - EXIT INT TERM
printf 'Stage 6 egress recovery verified: %s\n' "$RECEIPT"
