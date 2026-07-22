#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  generate_algolia_corpus.sh documents --corpus-size <integer-at-least-2>
  generate_algolia_corpus.sh manifest --corpus-size <integer-at-least-2>
EOF
}

die() {
  printf 'ERROR: %s\n' "$1" >&2
  exit "${2:-1}"
}

parse_args() {
  MODE="${1:-}"
  [ "$#" -gt 0 ] && shift
  CORPUS_SIZE=""

  while [ "$#" -gt 0 ]; do
    case "$1" in
      --corpus-size)
        CORPUS_SIZE="${2:-}"
        shift 2
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        usage >&2
        die "unknown argument: $1" 2
        ;;
    esac
  done

  case "$MODE" in
    documents|manifest) ;;
    *)
      usage >&2
      die "mode must be documents or manifest" 2
      ;;
  esac
  [[ "$CORPUS_SIZE" =~ ^[1-9][0-9]*$ ]] || die "--corpus-size must be a positive integer" 2
  [ "$CORPUS_SIZE" -ge 2 ] || die "--corpus-size must be at least 2 for scale probe coverage" 2
}

document_json_for() {
  local number="$1" object_id category name description price popularity color
  printf -v object_id 'scale-%06d' "$number"

  case "$number" in
    1)
      name="Alpha Scale Jacket"
      description="Known scale answer alpha settings proof trail rain"
      category="jackets"
      price=101
      popularity=100
      color="red"
      ;;
    2)
      name="Beta Scale Trainer"
      description="Known scale answer beta settings proof trail rain"
      category="shoes"
      price=82
      popularity=110
      color="blue"
      ;;
    *)
      category="$([ $((number % 2)) -eq 0 ] && printf 'shoes' || printf 'jackets')"
      color="$([ $((number % 3)) -eq 0 ] && printf 'green' || printf 'black')"
      name="Scale Product ${number}"
      description="Deterministic scale corpus product ${number}"
      price=$((50 + (number % 250)))
      popularity=$((CORPUS_SIZE - number + 1))
      ;;
  esac

  jq -cn \
    --arg objectID "$object_id" \
    --arg name "$name" \
    --arg description "$description" \
    --arg category "$category" \
    --arg color "$color" \
    --argjson price "$price" \
    --argjson popularity "$popularity" \
    '{objectID:$objectID,name:$name,description:$description,category:$category,color:$color,price:$price,popularity:$popularity}'
}

emit_documents() {
  local current=1
  while [ "$current" -le "$CORPUS_SIZE" ]; do
    document_json_for "$current"
    current=$((current + 1))
  done
}

source_configuration_json() {
  local rule_object_id="$1"
  jq -cn --arg rule_object_id "$rule_object_id" --argjson pagination_limited_to "$CORPUS_SIZE" '
    {
      settings:{
        searchableAttributes:["name","description","category"],
        customRanking:["desc(popularity)"],
        attributesForFaceting:["category","color"],
        paginationLimitedTo:$pagination_limited_to
      },
      synonyms:[
        {objectID:"synonym-trainer",type:"synonym",synonyms:["trainer","sneaker"]}
      ],
      rules:[
        {
          objectID:"rule-promote",
          conditions:[{pattern:"trail",anchoring:"is"}],
          consequence:{promote:[{objectID:$rule_object_id,position:0}]}
        },
        {
          objectID:"rule-hide",
          conditions:[{pattern:"rain",anchoring:"is"}],
          consequence:{hide:[{objectID:$rule_object_id}]}
        }
      ]
    }'
}

emit_manifest() {
  local first_id final_id known_answers source_configuration
  local jackets shoes red blue green black
  known_answers="$(document_json_for 1; document_json_for 2)"
  known_answers="$(printf '%s\n' "$known_answers" | jq -s '.')"
  first_id="$(printf '%s\n' "$known_answers" | jq -r '.[0].objectID')"
  printf -v final_id 'scale-%06d' "$CORPUS_SIZE"
  shoes=$((CORPUS_SIZE / 2))
  jackets=$((CORPUS_SIZE - shoes))
  red=1
  blue=1
  green=$((CORPUS_SIZE / 3))
  black=$((CORPUS_SIZE - red - blue - green))
  source_configuration="$(source_configuration_json "$first_id")"

  jq -n --argjson source_count "$CORPUS_SIZE" --argjson known_answers "$known_answers" \
    --argjson source_configuration "$source_configuration" --arg final_id "$final_id" \
    --argjson jackets "$jackets" --argjson shoes "$shoes" \
    --argjson red "$red" --argjson blue "$blue" --argjson green "$green" --argjson black "$black" '
    ($known_answers[0].objectID) as $first_id
    | ($known_answers[1].objectID) as $second_id
    | ($source_configuration.rules[] | select(.objectID == "rule-promote")) as $promotion_rule
    | ($source_configuration.rules[] | select(.objectID == "rule-hide")) as $hiding_rule
    | {
      source_configuration:$source_configuration,
      source_count:$source_count,
      synonym_count:($source_configuration.synonyms | length),
      rule_count:($source_configuration.rules | length),
      aggregate_expectations:{
        final_object_id:$final_id,
        facets:{
          category:{jackets:$jackets,shoes:$shoes},
          color:{black:$black,blue:$blue,green:$green,red:$red}
        }
      },
      known_answers_query:"Known answer",
      known_answers:$known_answers,
      probes:{
        settings:{
          request:{query:"settings proof",hitsPerPage:2},
          expected_object_ids:[$second_id,$first_id]
        },
        synonym:{
          request:{query:"sneaker",hitsPerPage:2},
          expected_object_ids:[$second_id]
        },
        promotion:{
          request:{query:$promotion_rule.conditions[0].pattern,hitsPerPage:2},
          expected_first_object_id:$first_id,
          competitor_object_id:$second_id,
          expected_rule_id:$promotion_rule.objectID
        },
        hiding:{
          request:{query:$hiding_rule.conditions[0].pattern,hitsPerPage:2},
          hidden_object_id:$first_id,
          expected_object_ids:[$second_id],
          expected_rule_id:$hiding_rule.objectID
        }
      }
    }
  '
}

main() {
  parse_args "$@"
  require_tool jq
  if [ "$MODE" = "documents" ]; then
    emit_documents
  else
    emit_manifest
  fi
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"
}

main "$@"
