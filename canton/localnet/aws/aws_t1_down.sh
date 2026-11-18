#!/usr/bin/env bash
# Deletes each AWS T1 resource with the tag Project=canton-gateway-t1, in dependency order, then verifies by tag
# that no tagged resource remains.
# Usage: aws_t1_down.sh
set -euo pipefail
# shellcheck source=aws_t1_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/aws_t1_lib.sh"

readonly LIVE_INSTANCE_STATES="Name=instance-state-name,Values=pending,running,stopping,stopped,shutting-down"

start_evidence_log aws_t1_down "aws_t1_down.sh" "$@"

# tagged_ids <describe-command> <jq-path> [extra filters...]
tagged_ids() {
  local describe_command="$1" id_path="$2"
  shift 2
  aws_ec2 "$describe_command" --filters "$PROJECT_TAG_FILTER" "$@" | jq -r "[$id_path] | .[]"
}

delete_instances() {
  local instance_ids
  mapfile -t instance_ids < <(tagged_ids describe-instances '.Reservations[].Instances[].InstanceId' "$LIVE_INSTANCE_STATES")
  if (( ${#instance_ids[@]} > 0 )); then
    echo "terminate instances ${instance_ids[*]}"
    aws_ec2 terminate-instances --instance-ids "${instance_ids[@]}" > /dev/null
    aws_ec2 wait instance-terminated --instance-ids "${instance_ids[@]}"
  fi
}

delete_network() {
  local vpc_id group_id subnet_id route_table_id association_id internet_gateway_id allocation_id volume_id
  for volume_id in $(tagged_ids describe-volumes '.Volumes[].VolumeId'); do
    echo "delete volume $volume_id"
    aws_ec2 delete-volume --volume-id "$volume_id"
  done
  for allocation_id in $(tagged_ids describe-addresses '.Addresses[].AllocationId'); do
    echo "release address $allocation_id"
    aws_ec2 release-address --allocation-id "$allocation_id"
  done
  # participant-sg refers to gateway-sg: revoke each ingress rule first.
  for group_id in $(tagged_ids describe-security-groups '.SecurityGroups[].GroupId'); do
    local rule_ids
    rule_ids="$(aws_ec2 describe-security-group-rules --filters "Name=group-id,Values=$group_id" \
      | jq -r '[.SecurityGroupRules[] | select(.IsEgress == false) | .SecurityGroupRuleId] | join(" ")')"
    if [[ -n "$rule_ids" ]]; then
      # shellcheck disable=SC2086 # one argument per rule id
      aws_ec2 revoke-security-group-ingress --group-id "$group_id" --security-group-rule-ids $rule_ids > /dev/null
    fi
  done
  for group_id in $(tagged_ids describe-security-groups '.SecurityGroups[].GroupId'); do
    echo "delete security group $group_id"
    aws_ec2 delete-security-group --group-id "$group_id"
  done
  for route_table_id in $(tagged_ids describe-route-tables '.RouteTables[].RouteTableId'); do
    for association_id in $(aws_ec2 describe-route-tables --route-table-ids "$route_table_id" \
        | jq -r '.RouteTables[0].Associations[] | select(.Main == false) | .RouteTableAssociationId'); do
      aws_ec2 disassociate-route-table --association-id "$association_id"
    done
    echo "delete route table $route_table_id"
    aws_ec2 delete-route-table --route-table-id "$route_table_id"
  done
  for subnet_id in $(tagged_ids describe-subnets '.Subnets[].SubnetId'); do
    echo "delete subnet $subnet_id"
    aws_ec2 delete-subnet --subnet-id "$subnet_id"
  done
  for internet_gateway_id in $(tagged_ids describe-internet-gateways '.InternetGateways[].InternetGatewayId'); do
    for vpc_id in $(aws_ec2 describe-internet-gateways --internet-gateway-ids "$internet_gateway_id" \
        | jq -r '.InternetGateways[0].Attachments[].VpcId'); do
      aws_ec2 detach-internet-gateway --internet-gateway-id "$internet_gateway_id" --vpc-id "$vpc_id"
    done
    echo "delete internet gateway $internet_gateway_id"
    aws_ec2 delete-internet-gateway --internet-gateway-id "$internet_gateway_id"
  done
  for vpc_id in $(tagged_ids describe-vpcs '.Vpcs[].VpcId'); do
    echo "delete vpc $vpc_id"
    aws_ec2 delete-vpc --vpc-id "$vpc_id"
  done
}

delete_key_pair() {
  local key_pair_id
  for key_pair_id in $(tagged_ids describe-key-pairs '.KeyPairs[].KeyPairId'); do
    echo "delete key pair $key_pair_id"
    aws_ec2 delete-key-pair --key-pair-id "$key_pair_id" > /dev/null
  done
  rm -f "$SSH_KEY_FILE"
}

# verify_none_remain: prints the count of each tagged resource kind. Exits non-zero unless each count is 0.
verify_none_remain() {
  local remaining=0 kind count
  echo "== Verification by tag $PROJECT_TAG_FILTER"
  for kind in "describe-instances|.Reservations[].Instances[].InstanceId|$LIVE_INSTANCE_STATES" \
      "describe-volumes|.Volumes[].VolumeId|" "describe-addresses|.Addresses[].AllocationId|" \
      "describe-network-interfaces|.NetworkInterfaces[].NetworkInterfaceId|" \
      "describe-security-groups|.SecurityGroups[].GroupId|" "describe-subnets|.Subnets[].SubnetId|" \
      "describe-route-tables|.RouteTables[].RouteTableId|" \
      "describe-internet-gateways|.InternetGateways[].InternetGatewayId|" "describe-vpcs|.Vpcs[].VpcId|" \
      "describe-key-pairs|.KeyPairs[].KeyPairId|"; do
    IFS='|' read -r describe_command id_path extra_filter <<<"$kind"
    if [[ -n "$extra_filter" ]]; then
      count="$(tagged_ids "$describe_command" "$id_path" "$extra_filter" | grep -c . || true)"
    else
      count="$(tagged_ids "$describe_command" "$id_path" | grep -c . || true)"
    fi
    echo "${describe_command#describe-}: $count"
    remaining=$((remaining + count))
  done
  echo "terminated tagged instances (kept by EC2 for about one hour, no charge): $(tagged_ids describe-instances \
    '.Reservations[].Instances[] | "\(.InstanceId)=\(.State.Name)"' Name=instance-state-name,Values=terminated | tr '\n' ' ')"
  echo "resource groups tagging API: $(aws "${AWS_ARGS[@]}" resourcegroupstaggingapi get-resources \
    --tag-filters "Key=Project,Values=$PROJECT_TAG_VALUE" | jq -c '[.ResourceTagMappingList[].ResourceARN]')"
  if (( remaining > 0 )); then
    echo "aws_t1_down.sh: $remaining tagged resources remain" >&2
    exit 1
  fi
  echo "aws_t1_down.sh: 0 tagged resources remain"
}

delete_instances
delete_network
delete_key_pair
verify_none_remain
