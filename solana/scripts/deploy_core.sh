set -euo pipefail

INIT_SIGNERS_CSV=""
program_keypair=""
skip_build=false
payer_keypair=""
RPC=https://api.devnet.solana.com

while getopts "p:k:g:u:s" opt; do
  case $opt in
    p)
        program_keypair=$OPTARG
        ;;
    k)
        payer_keypair=$OPTARG
        ;;
    s)
        skip_build=true
        ;;
    u)
        RPC=$OPTARG
        ;;
    g)
        INIT_SIGNERS_CSV=$OPTARG
        ;;
    \?)
        echo "Invalid option: -$OPTARG" >&2
        ;;
  esac
done

if [ -z "$INIT_SIGNERS_CSV" ]; then
    echo "Please provide the initial guardian set, separated by commas"
    exit 1
fi

# if program_keypair is not provided, create a new one with the solana cli
if [ -z "$program_keypair" ]; then
    program_keypair=$(solana-keygen grind --starts-with "wc:1" --ignore-case)
fi

if [ -z "$payer_keypair" ]; then
    echo "Please provide the payer keypair" >&2
    exit 1
fi

address=$(solana address --keypair $program_keypair)

ARTIFACTS_DIR=artifacts-$address

echo "Deploying at address $address"

pushd "$(dirname "$0")/.."

read -p "Do you want to deploy the program? (y/n) " -n 1 -r
echo ""

if [ "$skip_build" = false ]; then
  docker build -f Dockerfile --target export-stage --build-arg BRIDGE_ADDRESS=$address -o $ARTIFACTS_DIR .
else
  echo "Skipping build"
  # check artifacts folder exists
  if [ ! -d "$ARTIFACTS_DIR" ]; then
      echo "Artifacts folder not found. Exiting"
      exit 1
  fi
fi

docker build -f Dockerfile.client --target client .. --tag solana-cli

# NOTE: we deploy using a host solana cli, because the one in the docker image is too old, and can't actually land transactions
solana program deploy -u $RPC $ARTIFACTS_DIR/bridge.so --program-id $program_keypair --keypair $payer_keypair --with-compute-unit-price 50000

export SOLANA_KEY_TESTNET=$(worm solana private-key-base58 --keypair $payer_keypair)
# TODO: hardcoded testnet
worm solana init-wormhole --network testnet --guardian-address $INIT_SIGNERS_CSV --contract-address $address
popd
