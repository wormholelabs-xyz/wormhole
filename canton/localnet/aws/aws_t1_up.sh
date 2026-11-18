#!/usr/bin/env bash
# AWS T1: the disclosure service on a public gateway instance, the LocalNet participant on a private instance.
# Usage: aws_t1_up.sh infra|certs|participant|gateway|all
#   infra        VPC, public subnet, internet gateway, route table, two security groups, key pair, two instances.
#   certs        Throwaway CA, the proxy server certificate, the gateway client certificate and a rogue CA client.
#   participant  Packages, LocalNet and NTT 0.4.0, the mutual-TLS proxy. Copies the LocalNet state back.
#   gateway      Packages, the disclosure service image and container.
# Each resource has the tags Project=canton-gateway-t1 and Owner=claude-session. A second run reuses tagged resources.
# Each instance runs `shutdown -h +360` at boot and terminates at shutdown.
#
# Environment:
#   AWS_T1_STATE_DIR (required)  See aws_t1_lib.sh.
#   AWS_T1_CUSTODY_TREE          Checkout with custody_host.sh (default ../custody-lab next to this repository).
#   AWS_T1_CUSTODY_COMMIT        Commit of that checkout to ship (default e43470d5c).
#   AWS_T1_NTT_DAR               ntt-0.4.0.dar (default ../ntt-custody/canton/ntt/.daml/dist/ntt-0.4.0.dar).
#   AWS_T1_DISCLOSURE_TREE       NTT checkout with canton/disclosure/service (default ../disclosure-gateway).
#   AWS_T1_DISCLOSURE_COMMIT     Commit of the disclosure service (default f1eb778).
set -euo pipefail
# shellcheck source=aws_t1_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/aws_t1_lib.sh"

SIBLING_DIR="$(cd "$REPO_ROOT/.." && pwd)"
readonly SIBLING_DIR
readonly CUSTODY_TREE="${AWS_T1_CUSTODY_TREE:-$SIBLING_DIR/custody-lab}"
readonly CUSTODY_COMMIT="${AWS_T1_CUSTODY_COMMIT:-e43470d5c}"
readonly NTT_DAR="${AWS_T1_NTT_DAR:-$SIBLING_DIR/ntt-custody/canton/ntt/.daml/dist/ntt-0.4.0.dar}"
readonly DISCLOSURE_TREE="${AWS_T1_DISCLOSURE_TREE:-$SIBLING_DIR/disclosure-gateway}"
readonly DISCLOSURE_COMMIT="${AWS_T1_DISCLOSURE_COMMIT:-f1eb778}"
readonly DISCLOSURE_IMAGE="wormhole-disclosure-service:t1-$DISCLOSURE_COMMIT"
readonly REMOTE_WORK_DIR="/home/$SSH_USER/w"
readonly SSH_READY_MAX_POLLS=60
readonly SSH_READY_POLL_SECONDS=5
readonly SETUP_MAX_POLLS=180
readonly SETUP_POLL_SECONDS=20
readonly HEALTHZ_MAX_POLLS=30
readonly CERT_DAYS=2
# Docker apt repository signing key (https://docs.docker.com/engine/install/ubuntu/).
readonly DOCKER_APT_KEY_FINGERPRINT="9DC858229FC7DD38854AE2D88D81803C0EBFCD88"

# --- infra ---

# tagged_id <describe-command> <jq-path> [extra filters...]: the one id with the project tag, or empty.
tagged_id() {
  local describe_command="$1" id_path="$2" ids
  shift 2
  ids="$(aws_ec2 "$describe_command" --filters "$PROJECT_TAG_FILTER" "$@" | jq -r "[$id_path] | .[]")"
  (( $(grep -c . <<<"$ids") <= 1 )) || fail "more than one tagged result of $describe_command $*: $ids"
  echo "$ids"
}

operator_ip() {
  local address
  address="$(curl --connect-timeout 5 --max-time 10 -sSf https://checkip.amazonaws.com | tr -d '[:space:]')"
  [[ "$address" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]] || fail "checkip.amazonaws.com returned '$address'"
  echo "$address"
}

ensure_key_pair() {
  if aws_ec2 describe-key-pairs --key-names "$KEY_PAIR_NAME" > /dev/null 2>&1; then
    [[ -f "$SSH_KEY_FILE" ]] || fail "key pair $KEY_PAIR_NAME exists, but $SSH_KEY_FILE does not; run aws_t1_down.sh"
    return
  fi
  (umask 077 && aws_ec2 create-key-pair --key-name "$KEY_PAIR_NAME" --key-type ed25519 --key-format pem \
    --tag-specifications "$(tag_specification key-pair "$KEY_PAIR_NAME")" | jq -er .KeyMaterial > "$SSH_KEY_FILE")
  echo "key pair $KEY_PAIR_NAME, private key $SSH_KEY_FILE"
}

ensure_vpc() {
  vpc_id="$(tagged_id describe-vpcs '.Vpcs[].VpcId')"
  if [[ -z "$vpc_id" ]]; then
    vpc_id="$(aws_ec2 create-vpc --cidr-block "$VPC_CIDR" --tag-specifications "$(tag_specification vpc t1-vpc)" \
      | jq -er .Vpc.VpcId)"
    aws_ec2 wait vpc-available --vpc-ids "$vpc_id"
  fi
  record_resource vpc_id "$vpc_id"

  internet_gateway_id="$(tagged_id describe-internet-gateways '.InternetGateways[].InternetGatewayId')"
  if [[ -z "$internet_gateway_id" ]]; then
    internet_gateway_id="$(aws_ec2 create-internet-gateway \
      --tag-specifications "$(tag_specification internet-gateway t1-igw)" | jq -er .InternetGateway.InternetGatewayId)"
  fi
  if [[ "$(aws_ec2 describe-internet-gateways --internet-gateway-ids "$internet_gateway_id" \
      | jq -r '.InternetGateways[0].Attachments | length')" == "0" ]]; then
    aws_ec2 attach-internet-gateway --internet-gateway-id "$internet_gateway_id" --vpc-id "$vpc_id"
  fi
  record_resource internet_gateway_id "$internet_gateway_id"

  subnet_id="$(tagged_id describe-subnets '.Subnets[].SubnetId')"
  if [[ -z "$subnet_id" ]]; then
    subnet_id="$(aws_ec2 create-subnet --vpc-id "$vpc_id" --cidr-block "$SUBNET_CIDR" \
      --tag-specifications "$(tag_specification subnet t1-public)" | jq -er .Subnet.SubnetId)"
  fi
  aws_ec2 modify-subnet-attribute --subnet-id "$subnet_id" --map-public-ip-on-launch
  record_resource subnet_id "$subnet_id"

  route_table_id="$(tagged_id describe-route-tables '.RouteTables[].RouteTableId')"
  if [[ -z "$route_table_id" ]]; then
    route_table_id="$(aws_ec2 create-route-table --vpc-id "$vpc_id" \
      --tag-specifications "$(tag_specification route-table t1-public)" | jq -er .RouteTable.RouteTableId)"
    aws_ec2 create-route --route-table-id "$route_table_id" --destination-cidr-block 0.0.0.0/0 \
      --gateway-id "$internet_gateway_id" > /dev/null
    aws_ec2 associate-route-table --route-table-id "$route_table_id" --subnet-id "$subnet_id" > /dev/null
  fi
  record_resource route_table_id "$route_table_id"
}

# ensure_security_group <name>: prints the group id.
ensure_security_group() {
  local group_id
  group_id="$(tagged_id describe-security-groups '.SecurityGroups[].GroupId' "Name=group-name,Values=$1")"
  if [[ -z "$group_id" ]]; then
    group_id="$(aws_ec2 create-security-group --group-name "$1" --description "canton-gateway-t1 $1" --vpc-id "$vpc_id" \
      --tag-specifications "$(tag_specification security-group "$1")" | jq -er .GroupId)"
  fi
  echo "$group_id"
}

# authorize_ingress <group-id> <ip-permissions-json>: a rule that exists already is not an error.
authorize_ingress() {
  local output
  if ! output="$(aws_ec2 authorize-security-group-ingress --group-id "$1" --ip-permissions "$2" 2>&1)"; then
    grep -q "InvalidPermission.Duplicate" <<<"$output" || fail "authorize-security-group-ingress $1: $output"
  fi
}

ensure_security_groups() {
  local operator_cidr="$1/32"
  gateway_sg_id="$(ensure_security_group gateway-sg)"
  participant_sg_id="$(ensure_security_group participant-sg)"
  record_resource gateway_sg_id "$gateway_sg_id"
  record_resource participant_sg_id "$participant_sg_id"
  authorize_ingress "$gateway_sg_id" "$(jq -cn --arg cidr "$operator_cidr" --argjson service "$SERVICE_PORT" '[
    {IpProtocol: "tcp", FromPort: 22, ToPort: 22, IpRanges: [{CidrIp: $cidr, Description: "operator ssh"}]},
    {IpProtocol: "tcp", FromPort: $service, ToPort: $service, IpRanges: [{CidrIp: $cidr, Description: "operator disclosure service"}]}]')"
  authorize_ingress "$participant_sg_id" "$(jq -cn --arg gateway "$gateway_sg_id" --argjson proxy "$PROXY_PORT" '[
    {IpProtocol: "tcp", FromPort: 22, ToPort: 22, UserIdGroupPairs: [{GroupId: $gateway, Description: "ssh from gateway"}]},
    {IpProtocol: "tcp", FromPort: $proxy, ToPort: $proxy, UserIdGroupPairs: [{GroupId: $gateway, Description: "mTLS proxy from gateway"}]}]')"
}

# ensure_instance <name> <type> <root-gib> <security-group-id>: prints the instance id.
ensure_instance() {
  local name="$1" instance_type="$2" root_gib="$3" group_id="$4" instance_id ami_id user_data_file
  instance_id="$(tagged_id describe-instances '.Reservations[].Instances[].InstanceId' "Name=tag:Name,Values=$name" \
    "Name=instance-state-name,Values=pending,running,stopping,stopped")"
  if [[ -z "$instance_id" ]]; then
    ami_id="$(aws "${AWS_ARGS[@]}" ssm get-parameter --name "$UBUNTU_AMI_PARAMETER" | jq -er .Parameter.Value)"
    user_data_file="$AWS_T1_STATE_DIR/user-data.sh"
    printf '#!/bin/bash\nshutdown -h +%s\n' "$SELF_TERMINATE_MINUTES" > "$user_data_file"
    instance_id="$(aws_ec2 run-instances --image-id "$ami_id" --instance-type "$instance_type" --count 1 \
      --key-name "$KEY_PAIR_NAME" --subnet-id "$subnet_id" --security-group-ids "$group_id" \
      --associate-public-ip-address --instance-initiated-shutdown-behavior terminate \
      --metadata-options HttpTokens=required,HttpEndpoint=enabled \
      --block-device-mappings "[{\"DeviceName\":\"/dev/sda1\",\"Ebs\":{\"VolumeSize\":$root_gib,\"VolumeType\":\"gp3\",\"DeleteOnTermination\":true}}]" \
      --user-data "file://$user_data_file" \
      --tag-specifications "$(tag_specification instance "$name")" "$(tag_specification volume "$name")" \
      | jq -er '.Instances[0].InstanceId')"
    echo "launched $name $instance_id ($instance_type, $ami_id)" >&2
  fi
  echo "$instance_id"
}

write_ssh_config() {
  cat > "$SSH_CONFIG_FILE" <<EOF
Host t1-gateway
  HostName $gateway_public_ip
Host t1-participant
  HostName $participant_private_ip
  ProxyJump t1-gateway
Host t1-*
  User $SSH_USER
  IdentityFile $SSH_KEY_FILE
  IdentitiesOnly yes
  UserKnownHostsFile $AWS_T1_STATE_DIR/known_hosts
  StrictHostKeyChecking accept-new
  ConnectTimeout 10
  ServerAliveInterval 30
  ExitOnForwardFailure yes
EOF
}

infra_up() {
  local operator_address instance_description
  operator_address="$(operator_ip)"
  record_resource operator_ip "$operator_address"
  echo "operator IP $operator_address"
  ensure_key_pair
  ensure_vpc
  ensure_security_groups "$operator_address"
  gateway_instance_id="$(ensure_instance t1-gateway "$GATEWAY_INSTANCE_TYPE" "$GATEWAY_ROOT_GIB" "$gateway_sg_id")"
  participant_instance_id="$(ensure_instance t1-participant "$PARTICIPANT_INSTANCE_TYPE" "$PARTICIPANT_ROOT_GIB" "$participant_sg_id")"
  record_resource gateway_instance_id "$gateway_instance_id"
  record_resource participant_instance_id "$participant_instance_id"
  aws_ec2 wait instance-running --instance-ids "$gateway_instance_id" "$participant_instance_id"
  instance_description="$(aws_ec2 describe-instances --instance-ids "$gateway_instance_id" "$participant_instance_id")"
  gateway_public_ip="$(jq -er --arg id "$gateway_instance_id" '.Reservations[].Instances[] | select(.InstanceId == $id) | .PublicIpAddress' <<<"$instance_description")"
  participant_public_ip="$(jq -er --arg id "$participant_instance_id" '.Reservations[].Instances[] | select(.InstanceId == $id) | .PublicIpAddress' <<<"$instance_description")"
  participant_private_ip="$(jq -er --arg id "$participant_instance_id" '.Reservations[].Instances[] | select(.InstanceId == $id) | .PrivateIpAddress' <<<"$instance_description")"
  record_resource gateway_public_ip "$gateway_public_ip"
  record_resource participant_public_ip "$participant_public_ip"
  record_resource participant_private_ip "$participant_private_ip"
  record_resource launched_at "$(jq -r '[.Reservations[].Instances[].LaunchTime] | min' <<<"$instance_description")"
  write_ssh_config
  cat "$AWS_T1_STATE_FILE"
}

# --- certs ---

certs_up() {
  local ca_name
  load_resources
  mkdir -p "$CERT_DIR"
  (
    umask 077
    for ca_name in ca rogue-ca; do
      openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days "$CERT_DAYS" \
        -subj "/CN=t1-aws-$ca_name" -keyout "$CERT_DIR/$ca_name.key" -out "$CERT_DIR/$ca_name.crt" 2> /dev/null
    done
    # issue_certificate <name> <ca-name> <extensions>
    issue_certificate() {
      openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=$1" \
        -keyout "$CERT_DIR/$1.key" -out "$CERT_DIR/$1.csr" 2> /dev/null
      openssl x509 -req -in "$CERT_DIR/$1.csr" -CA "$CERT_DIR/$2.crt" -CAkey "$CERT_DIR/$2.key" -CAcreateserial \
        -days "$CERT_DAYS" -extfile <(printf '%s\n' "$3") -out "$CERT_DIR/$1.crt" 2> /dev/null
      rm -f "$CERT_DIR/$1.csr"
    }
    issue_certificate "$PROXY_SERVER_NAME" ca "subjectAltName=DNS:$PROXY_SERVER_NAME,IP:$participant_private_ip
extendedKeyUsage=serverAuth"
    issue_certificate gateway-client ca "extendedKeyUsage=clientAuth"
    issue_certificate rogue-client rogue-ca "extendedKeyUsage=clientAuth"
  )
  openssl verify -CAfile "$CERT_DIR/ca.crt" "$CERT_DIR/$PROXY_SERVER_NAME.crt" "$CERT_DIR/gateway-client.crt"
  if openssl verify -CAfile "$CERT_DIR/ca.crt" "$CERT_DIR/rogue-client.crt" > /dev/null 2>&1; then
    fail "the rogue client certificate verifies against the T1 CA"
  fi
}

# --- instance provisioning ---

await_ssh() {
  local poll
  for (( poll = 1; poll <= SSH_READY_MAX_POLLS; poll++ )); do
    if ssh_t1 "$1" true 2> /dev/null; then
      ssh_t1 "$1" cloud-init status --wait > /dev/null || true
      echo "$1 SSH ready"
      return 0
    fi
    sleep "$SSH_READY_POLL_SECONDS"
  done
  fail "$1 SSH is not ready after $((SSH_READY_MAX_POLLS * SSH_READY_POLL_SECONDS)) s"
}

# install_packages <host> <extra apt packages...>: Ubuntu packages and Docker from the Docker apt repository.
install_packages() {
  local host="$1"
  shift
  # shellcheck disable=SC2029 # the arguments expand on the laptop on purpose
  ssh_t1 "$host" "sudo bash -s -- '$DOCKER_APT_KEY_FINGERPRINT' $*" <<'EOF'
set -euo pipefail
expected_fingerprint="$1"
shift
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q ca-certificates curl gnupg jq "$@" > /dev/null
install -m 0755 -d /etc/apt/keyrings
curl --max-time 60 -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
actual_fingerprint="$(gpg --show-keys --with-colons /etc/apt/keyrings/docker.asc | awk -F: '$1 == "fpr" {print $10; exit}')"
[[ "$actual_fingerprint" == "$expected_fingerprint" ]] || { echo "Docker apt key fingerprint $actual_fingerprint" >&2; exit 1; }
chmod a+r /etc/apt/keyrings/docker.asc
. /etc/os-release
echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu $VERSION_CODENAME stable" \
  > /etc/apt/sources.list.d/docker.list
apt-get update -q
apt-get install -y -q docker-ce docker-ce-cli containerd.io docker-compose-plugin > /dev/null
usermod -aG docker ubuntu
docker --version
docker compose version
EOF
}

# build_participant_bundle: the trees, DARs and certificates for the participant, as one tarball.
build_participant_bundle() {
  local bundle_dir="$AWS_T1_STATE_DIR/participant-bundle" dist_dir
  rm -rf "$bundle_dir"
  mkdir -p "$bundle_dir/wormhole" "$bundle_dir/custody-lab" "$bundle_dir/ntt/canton/ntt/.daml/dist" "$bundle_dir/t1-certs"
  git -C "$REPO_ROOT" archive HEAD canton node sdk wormchain | tar -x -C "$bundle_dir/wormhole"
  git -C "$CUSTODY_TREE" archive "$CUSTODY_COMMIT" canton | tar -x -C "$bundle_dir/custody-lab"
  for dist_dir in canton/core/.daml/dist canton/test/.daml/dist; do
    [[ -n "$(ls "$REPO_ROOT/$dist_dir"/*.dar 2> /dev/null)" ]] || fail "no DAR in $REPO_ROOT/$dist_dir; run dpm build --all in canton"
    mkdir -p "$bundle_dir/wormhole/$dist_dir" "$bundle_dir/custody-lab/$dist_dir"
    cp "$REPO_ROOT/$dist_dir"/*.dar "$bundle_dir/wormhole/$dist_dir/"
    cp "$REPO_ROOT/$dist_dir"/*.dar "$bundle_dir/custody-lab/$dist_dir/"
  done
  [[ -f "$NTT_DAR" ]] || fail "$NTT_DAR does not exist"
  cp "$NTT_DAR" "$bundle_dir/ntt/canton/ntt/.daml/dist/"
  cp "$CERT_DIR/ca.crt" "$CERT_DIR/$PROXY_SERVER_NAME.crt" "$CERT_DIR/$PROXY_SERVER_NAME.key" "$bundle_dir/t1-certs/"
  printf 'wormhole %s\ncustody-lab %s\n' "$(git -C "$REPO_ROOT" rev-parse HEAD)" \
    "$(git -C "$CUSTODY_TREE" rev-parse "$CUSTODY_COMMIT")" > "$bundle_dir/commits.txt"
  cat "$bundle_dir/commits.txt"
  COPYFILE_DISABLE=1 tar -czf "$AWS_T1_STATE_DIR/participant-bundle.tgz" -C "$bundle_dir" .
  rm -rf "$bundle_dir"
}

participant_up() {
  local poll status
  load_resources
  await_ssh t1-gateway
  await_ssh t1-participant
  install_packages t1-participant git openssl unzip nodejs openjdk-21-jre-headless golang-go netcat-openbsd
  build_participant_bundle
  ssh_t1 t1-participant "rm -rf '$REMOTE_WORK_DIR' && mkdir -p '$REMOTE_WORK_DIR'"
  scp_t1 "$AWS_T1_STATE_DIR/participant-bundle.tgz" "t1-participant:$REMOTE_WORK_DIR/"
  ssh_t1 t1-participant "cd '$REMOTE_WORK_DIR' && tar -xzf participant-bundle.tgz && rm participant-bundle.tgz && node --version && java -version 2>&1 | head -1"
  # A detached run: the setup takes longer than one SSH session should stay open.
  ssh_t1 t1-participant "cd '$REMOTE_WORK_DIR' && nohup bash -c \"bash wormhole/canton/localnet/aws/aws_t1_participant_setup.sh '$REMOTE_WORK_DIR' '$participant_private_ip'; echo \\\$? > setup.exit\" > setup.log 2>&1 < /dev/null &"
  for (( poll = 1; poll <= SETUP_MAX_POLLS; poll++ )); do
    sleep "$SETUP_POLL_SECONDS"
    ssh_t1 t1-participant "grep '^===' '$REMOTE_WORK_DIR/setup.log' | tail -n 1" || true
    status="$(ssh_t1 t1-participant "cat '$REMOTE_WORK_DIR/setup.exit' 2> /dev/null" || true)"
    [[ -z "$status" ]] || break
  done
  scp_t1 "t1-participant:$REMOTE_WORK_DIR/setup.log" "$EVIDENCE_DIR/aws_t1_participant_setup-$(date -u +%Y%m%dT%H%M%SZ).log"
  [[ "$status" == "0" ]] || fail "participant setup exit status '${status:-none}'; see the setup log in $EVIDENCE_DIR"
  rm -rf "$AWS_T1_STATE_DIR/localnet-state"
  ssh_t1 t1-participant "tar -czf - -C '$REMOTE_WORK_DIR' localnet-state" | tar -xzf - -C "$AWS_T1_STATE_DIR"
  echo "LocalNet state copied to $AWS_T1_STATE_DIR/localnet-state"
}

# --- gateway ---

await_gateway_healthz() {
  local poll
  for (( poll = 1; poll <= HEALTHZ_MAX_POLLS; poll++ )); do
    if curl --connect-timeout 5 --max-time 10 -sSf "http://$gateway_public_ip:$SERVICE_PORT/v1/healthz"; then
      echo
      return 0
    fi
    sleep 2
  done
  ssh_t1 t1-gateway "docker logs --tail 30 t1-disclosure-gateway" >&2 || true
  fail "the disclosure service is not healthy after $HEALTHZ_MAX_POLLS polls"
}

gateway_up() {
  local state_dir="$AWS_T1_STATE_DIR/localnet-state" gg_party custody_party
  load_resources
  [[ -f "$state_dir/gateway-t1/gateway.jwt" ]] || fail "no gateway.jwt; run aws_t1_up.sh participant"
  gg_party="$(jq -er .partyId "$state_dir/gg-artifacts.json")"
  custody_party="$(jq -er .partyId "$state_dir/custody-artifacts.json")"
  await_ssh t1-gateway
  install_packages t1-gateway netcat-openbsd
  git -C "$DISCLOSURE_TREE" archive "$DISCLOSURE_COMMIT:canton/disclosure/service" \
    | ssh_t1 t1-gateway "rm -rf ~/service && mkdir -p ~/service ~/t1 && tar -x -C ~/service"
  scp_t1 "$CERT_DIR/ca.crt" "$CERT_DIR/gateway-client.crt" "$CERT_DIR/gateway-client.key" \
    "$state_dir/gateway-t1/gateway.jwt" t1-gateway:t1/
  # Readable by the distroless nonroot user (65532).
  ssh_t1 t1-gateway "chmod 755 ~/t1 && chmod 644 ~/t1/*"
  ssh_t1 t1-gateway "docker build -q -t '$DISCLOSURE_IMAGE' ~/service"
  # shellcheck disable=SC2029 # the arguments expand on the laptop on purpose
  ssh_t1 t1-gateway "docker rm -f t1-disclosure-gateway > /dev/null 2>&1; docker run -d --name t1-disclosure-gateway \
    --restart unless-stopped -p $SERVICE_PORT:$SERVICE_PORT -v \$HOME/t1:/etc/disclosure:ro '$DISCLOSURE_IMAGE' \
    --json-api 'https://$participant_private_ip:$PROXY_PORT' \
    --json-api-ca-file /etc/disclosure/ca.crt \
    --json-api-client-cert-file /etc/disclosure/gateway-client.crt \
    --json-api-client-key-file /etc/disclosure/gateway-client.key \
    --access-token-file /etc/disclosure/gateway.jwt \
    --disclosing-party '$gg_party,$custody_party' \
    --listen 0.0.0.0:$SERVICE_PORT"
  await_gateway_healthz
  echo "aws_t1_up.sh gateway: service http://$gateway_public_ip:$SERVICE_PORT, image $DISCLOSURE_IMAGE"
}

start_evidence_log aws_t1_up "aws_t1_up.sh" "$@"
case "${1:-}" in
  infra) infra_up ;;
  certs) certs_up ;;
  participant) participant_up ;;
  gateway) gateway_up ;;
  all) infra_up; certs_up; participant_up; gateway_up ;;
  *) echo "usage: aws_t1_up.sh infra|certs|participant|gateway|all" >&2; exit 2 ;;
esac
