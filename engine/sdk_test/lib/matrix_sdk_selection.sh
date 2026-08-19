# Validate MATRIX_SDKS against DEFAULT_SDKS and populate the SDKS array.
sdk_is_allowed() {
  local sdk="$1"
  local candidate

  for candidate in "${DEFAULT_SDKS[@]}"; do
    if [ "$candidate" = "$sdk" ]; then
      return 0
    fi
  done

  return 1
}

# TODO: Document configure_sdks.
configure_sdks() {
  local requested="${MATRIX_SDKS:-}"
  local normalized sdk
  local selected=()

  if [ -z "$requested" ]; then
    return
  fi

  normalized="${requested//,/ }"
  for sdk in $normalized; do
    if ! sdk_is_allowed "$sdk"; then
      echo "FATAL: Unknown SDK in MATRIX_SDKS: $sdk" >&2
      echo "Allowed SDKs: ${DEFAULT_SDKS[*]}" >&2
      exit 2
    fi

    selected+=("$sdk")
  done

  if [ "${#selected[@]}" -eq 0 ]; then
    echo "FATAL: MATRIX_SDKS did not select any SDKs" >&2
    exit 2
  fi

  SDKS=("${selected[@]}")
}

run_sdk() {
  case "$1" in
    js) run_js ;;
    go) run_go ;;
    python) run_python ;;
    ruby) run_ruby ;;
    php) run_php ;;
    java) run_java ;;
    swift) run_swift ;;
    *)
      echo "FATAL: Unsupported SDK runner: $1" >&2
      exit 2
      ;;
  esac
}
