# Shared settings of the AWS T1 scripts. Source this file; do not run it.
# shellcheck shell=bash disable=SC2034
#
# AWS_T1_STATE_DIR (required): local directory, mode 0700, for the SSH key, certificates, the resource state file and
# the LocalNet state copy. Keep it outside every repository.

AWS_T1_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCALNET_SCRIPT_DIR="$(cd "$AWS_T1_SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$LOCALNET_SCRIPT_DIR/../.." && pwd)"

readonly AWS_ARGS=(--profile "${AWS_T1_PROFILE:-personal}" --region "${AWS_T1_REGION:-us-east-1}" --output json)
readonly PROJECT_TAG_VALUE="canton-gateway-t1"
readonly OWNER_TAG_VALUE="claude-session"
readonly PROJECT_TAG_FILTER="Name=tag:Project,Values=$PROJECT_TAG_VALUE"
readonly KEY_PAIR_NAME="$PROJECT_TAG_VALUE"
readonly VPC_CIDR="10.71.0.0/16"
readonly SUBNET_CIDR="10.71.1.0/24"
readonly PARTICIPANT_INSTANCE_TYPE="m7i.2xlarge"
readonly GATEWAY_INSTANCE_TYPE="t3.small"
readonly PARTICIPANT_ROOT_GIB=60
readonly GATEWAY_ROOT_GIB=20
readonly SELF_TERMINATE_MINUTES=360
readonly UBUNTU_AMI_PARAMETER="/aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id"
readonly SSH_USER="ubuntu"
readonly PROXY_PORT=7443
readonly SERVICE_PORT=7599
readonly PROXY_SERVER_NAME="t1-ledger-proxy"
# Laptop ports of the tunnels: LocalNet host port + LOCAL_TUNNEL_PORT_OFFSET, so that a local LocalNet keeps its ports.
readonly LOCAL_TUNNEL_PORT_OFFSET=10000
readonly LOCAL_PROXY_TUNNEL_PORT=17443
# Participant ports that the gateway must find closed: LocalNet APIs, admin APIs, Scan, PostgreSQL.
readonly PARTICIPANT_CLOSED_PORTS=(2901 2902 2903 2975 3901 3902 3903 3975 4901 4902 4903 4975 4000 5432 5440 80 8080)

[[ -n "${AWS_T1_STATE_DIR:-}" ]] || { echo "set AWS_T1_STATE_DIR to a directory outside the repository" >&2; exit 2; }
mkdir -p "$AWS_T1_STATE_DIR"
chmod 700 "$AWS_T1_STATE_DIR"
case "$(cd "$AWS_T1_STATE_DIR" && pwd)/" in
  "$REPO_ROOT"/*) echo "AWS_T1_STATE_DIR is inside the repository: $AWS_T1_STATE_DIR" >&2; exit 2 ;;
esac
readonly AWS_T1_STATE_FILE="$AWS_T1_STATE_DIR/resources.env"
readonly SSH_KEY_FILE="$AWS_T1_STATE_DIR/$KEY_PAIR_NAME.pem"
readonly SSH_CONFIG_FILE="$AWS_T1_STATE_DIR/ssh_config"
readonly CERT_DIR="$AWS_T1_STATE_DIR/certs"
readonly EVIDENCE_DIR="${AWS_T1_EVIDENCE_DIR:-$(cd "$REPO_ROOT/.." && pwd)/localnet-state/evidence}"

aws_ec2() { aws "${AWS_ARGS[@]}" ec2 "$@"; }

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# tag_specification <resource-type> <name>
tag_specification() {
  echo "ResourceType=$1,Tags=[{Key=Project,Value=$PROJECT_TAG_VALUE},{Key=Owner,Value=$OWNER_TAG_VALUE},{Key=Name,Value=$2}]"
}

# record_resource <name> <value>: sets the variable in the state file.
record_resource() {
  [[ "$1" =~ ^[a-z_]+$ ]] || fail "bad state name $1"
  touch "$AWS_T1_STATE_FILE"
  grep -v "^$1=" "$AWS_T1_STATE_FILE" > "$AWS_T1_STATE_FILE.new" || true
  printf '%s=%q\n' "$1" "$2" >> "$AWS_T1_STATE_FILE.new"
  mv "$AWS_T1_STATE_FILE.new" "$AWS_T1_STATE_FILE"
}

load_resources() {
  [[ -f "$AWS_T1_STATE_FILE" ]] || fail "$AWS_T1_STATE_FILE does not exist; run aws_t1_up.sh infra"
  # shellcheck disable=SC1090 # written by record_resource
  source "$AWS_T1_STATE_FILE"
}

# start_evidence_log <script-name> <args...>: appends stdout and stderr to an evidence log.
start_evidence_log() {
  mkdir -p "$EVIDENCE_DIR"
  local log_file
  log_file="$EVIDENCE_DIR/$1-$(date -u +%Y%m%dT%H%M%SZ).log"
  exec > >(tee -a "$log_file") 2> >(tee -a "$log_file" >&2)
  echo "Run $* at $(date -u +%Y-%m-%dT%H:%M:%SZ), commit $(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "Evidence log: $log_file"
}

# ssh_t1 <t1-gateway|t1-participant> <command...>
ssh_t1() {
  ssh -F "$SSH_CONFIG_FILE" "$@"
}

scp_t1() {
  scp -F "$SSH_CONFIG_FILE" -q "$@"
}
