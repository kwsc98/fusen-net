#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0 OR MIT

set -euo pipefail
umask 077

output_dir="${1:-configs/certs}"
server_name="${2:-localhost}"

if ! command -v openssl >/dev/null 2>&1; then
  echo "openssl is required" >&2
  exit 1
fi

if (( ${#server_name} == 0 || ${#server_name} > 253 )) \
  || [[ ! "$server_name" =~ ^[A-Za-z0-9][A-Za-z0-9.-]*$ ]] \
  || [[ "$server_name" == *. ]]; then
  echo "server name must be an ASCII DNS name or IPv4 address accepted by Stellaris" >&2
  exit 1
fi

valid_ipv4=false
if [[ "$server_name" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; then
  IFS='.' read -r octet1 octet2 octet3 octet4 <<<"$server_name"
  if (( 10#$octet1 <= 255 && 10#$octet2 <= 255 && 10#$octet3 <= 255 && 10#$octet4 <= 255 )) \
    && [[ "$octet1" == "$((10#$octet1))" \
      && "$octet2" == "$((10#$octet2))" \
      && "$octet3" == "$((10#$octet3))" \
      && "$octet4" == "$((10#$octet4))" ]]; then
    octet1=$((10#$octet1))
    octet2=$((10#$octet2))
    octet3=$((10#$octet3))
    octet4=$((10#$octet4))
    valid_ipv4=true
    if (( (octet1 == 0 && octet2 == 0 && octet3 == 0 && octet4 == 0)
      || (octet1 >= 224 && octet1 <= 239)
      || (octet1 == 255 && octet2 == 255 && octet3 == 255 && octet4 == 255) )); then
      echo "server IPv4 address must be unicast and non-unspecified" >&2
      exit 1
    fi
  fi
fi

if [[ "$valid_ipv4" != true ]]; then
  if [[ "$server_name" =~ ^[0-9.]+$ ]]; then
    echo "numeric server name must be a canonical IPv4 address" >&2
    exit 1
  fi
  IFS='.' read -r -a dns_labels <<<"$server_name"
  for label in "${dns_labels[@]}"; do
    if (( ${#label} == 0 || ${#label} > 63 )) \
      || [[ ! "$label" =~ ^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?$ ]]; then
      echo "server name contains an invalid DNS label" >&2
      exit 1
    fi
  done
  final_label="${dns_labels[${#dns_labels[@]} - 1]}"
  if [[ ! "$final_label" =~ [A-Za-z] ]]; then
    echo "final DNS label must contain an ASCII letter" >&2
    exit 1
  fi
fi

files=(
  deployment-ca.pem
  deployment-ca-key.pem
  deployment-server.pem
  deployment-server-key.pem
)
for file in "${files[@]}"; do
  if [[ -e "$output_dir/$file" ]]; then
    echo "refusing to overwrite $output_dir/$file" >&2
    exit 1
  fi
done

mkdir -p "$output_dir"
chmod 0700 "$output_dir"
temp_dir="$(mktemp -d)"
cleanup() {
  rm -rf -- "$temp_dir"
}
trap cleanup EXIT

if [[ "$valid_ipv4" == true ]]; then
  subject_alt_name="IP:$server_name"
else
  subject_alt_name="DNS:$server_name"
fi

openssl req -x509 -new -newkey ec \
  -pkeyopt ec_paramgen_curve:prime256v1 \
  -nodes -sha256 -days 30 \
  -subj "/CN=Stellaris Development CA" \
  -keyout "$temp_dir/deployment-ca-key.pem" \
  -out "$temp_dir/deployment-ca.pem"

openssl req -new -newkey ec \
  -pkeyopt ec_paramgen_curve:prime256v1 \
  -nodes -sha256 \
  -subj "/CN=$server_name" \
  -keyout "$temp_dir/deployment-server-key.pem" \
  -out "$temp_dir/deployment-server.csr"

{
  echo "[server]"
  echo "basicConstraints=critical,CA:FALSE"
  echo "keyUsage=critical,digitalSignature"
  echo "extendedKeyUsage=serverAuth"
  echo "subjectAltName=$subject_alt_name"
} > "$temp_dir/server.ext"

openssl x509 -req \
  -in "$temp_dir/deployment-server.csr" \
  -CA "$temp_dir/deployment-ca.pem" \
  -CAkey "$temp_dir/deployment-ca-key.pem" \
  -CAserial "$temp_dir/deployment-ca.srl" \
  -CAcreateserial -sha256 -days 7 \
  -extfile "$temp_dir/server.ext" -extensions server \
  -out "$temp_dir/deployment-server.pem"

openssl verify \
  -CAfile "$temp_dir/deployment-ca.pem" \
  "$temp_dir/deployment-server.pem"

install -m 0600 "$temp_dir/deployment-ca-key.pem" "$output_dir/deployment-ca-key.pem"
install -m 0600 "$temp_dir/deployment-server-key.pem" "$output_dir/deployment-server-key.pem"
install -m 0644 "$temp_dir/deployment-ca.pem" "$output_dir/deployment-ca.pem"
install -m 0644 "$temp_dir/deployment-server.pem" "$output_dir/deployment-server.pem"

echo "created development-only TLS material in $output_dir for $server_name"
echo "keep deployment-ca-key.pem offline from Stellaris and do not use these files in production"
