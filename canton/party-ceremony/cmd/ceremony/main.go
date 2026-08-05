// Command ceremony drives guardian party ceremonies: multi-actor, resumable
// creation of the guardian decentralized namespace and the
// guardianGovernance / guardianObserver parties.
//
// Exit codes follow the ceremony convention: 0 = complete, 1 = error,
// 2 = progress recorded but more actors must resume before completion.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"sort"
	"strings"

	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony"
	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony/fake"
	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/sign"
	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/store"
	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/topology/console"
)

func main() {
	if len(os.Args) < 2 {
		usage()
		os.Exit(1)
	}
	var err error
	switch os.Args[1] {
	case "keygen":
		err = runKeygen(os.Args[2:])
	case "sign":
		err = runSign(os.Args[2:])
	case "init":
		err = runInit(os.Args[2:])
	case "resume":
		err = runResume(os.Args[2:])
	case "status":
		err = runStatus(os.Args[2:])
	default:
		usage()
		os.Exit(1)
	}
	if err != nil {
		if err == errWaiting {
			os.Exit(2)
		}
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}

var errWaiting = fmt.Errorf("ceremony waiting on other actors")

func usage() {
	fmt.Fprintln(os.Stderr, strings.TrimSpace(`
usage:
  ceremony keygen  --out <prefix>
  ceremony sign    --key <file> <hashHex>
  ceremony init    --dir <ceremony-dir> --id <workflow-id> --threshold <k> --coordinator <owner-id> --owner <id>=<pubfile> [--owner ...]
  ceremony resume  --dir <ceremony-dir> --actor <owner-id> (--key <file> | --sign-cmd <cmd>) [--backend fake --ledger <file> | --backend console --ops-script <ops.canton>]
  ceremony status  --dir <ceremony-dir>

exit codes: 0 complete · 1 error · 2 waiting on other actors`))
}

func runKeygen(args []string) error {
	fs := flag.NewFlagSet("keygen", flag.ContinueOnError)
	out := fs.String("out", "", "output path prefix (<prefix>.key / <prefix>.pub)")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *out == "" {
		return fmt.Errorf("keygen: --out is required")
	}
	pubDER, err := sign.GenerateKeyFiles(*out)
	if err != nil {
		return err
	}
	fmt.Printf("wrote %s.key and %s.pub (fingerprint %s)\n", *out, *out, sign.PublicKeyFingerprint(pubDER)[:16])
	return nil
}

func runSign(args []string) error {
	fs := flag.NewFlagSet("sign", flag.ContinueOnError)
	key := fs.String("key", "", "PKCS#8 DER Ed25519 private key file")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *key == "" || fs.NArg() != 1 {
		return fmt.Errorf("sign: --key and exactly one hashHex argument are required")
	}
	signer, err := sign.NewKeySigner(*key)
	if err != nil {
		return err
	}
	sig, err := signer.Sign(fs.Arg(0))
	if err != nil {
		return err
	}
	fmt.Println(sig)
	return nil
}

type ownerFlags []string

func (o *ownerFlags) String() string     { return strings.Join(*o, ",") }
func (o *ownerFlags) Set(v string) error { *o = append(*o, v); return nil }

func runInit(args []string) error {
	fs := flag.NewFlagSet("init", flag.ContinueOnError)
	dir := fs.String("dir", "", "ceremony directory")
	id := fs.String("id", "", "workflow id")
	threshold := fs.Int("threshold", 0, "signing/authorization threshold")
	coordinator := fs.String("coordinator", "", "coordinating owner id")
	var owners ownerFlags
	fs.Var(&owners, "owner", "owner as <id>=<pubkey-der-file>, repeatable")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *dir == "" || *id == "" || len(owners) == 0 {
		return fmt.Errorf("init: --dir, --id and at least one --owner are required")
	}
	specOwners := make([]ceremony.Owner, 0, len(owners))
	for _, entry := range owners {
		id, file, ok := strings.Cut(entry, "=")
		if !ok {
			return fmt.Errorf("init: malformed --owner %q, want <id>=<pubfile>", entry)
		}
		pub, err := os.ReadFile(file)
		if err != nil {
			return fmt.Errorf("init: reading owner key %s: %w", file, err)
		}
		if err := sign.ParsePublicKeyDER(pub); err != nil {
			return fmt.Errorf("init: owner %q key %s: %w", id, file, err)
		}
		specOwners = append(specOwners, ceremony.Owner{ID: id, PublicKeyDER: pub})
	}
	spec, err := ceremony.NewOnboardingSpec(*id, *coordinator, *threshold, specOwners)
	if err != nil {
		return err
	}
	if _, err := store.Init(*dir, spec); err != nil {
		return err
	}
	fmt.Printf("ceremony %s initialized in %s (%d owners, threshold %d)\n", *id, *dir, len(specOwners), *threshold)
	return nil
}

func runResume(args []string) error {
	fs := flag.NewFlagSet("resume", flag.ContinueOnError)
	dir := fs.String("dir", "", "ceremony directory")
	actor := fs.String("actor", "", "this operator's owner id")
	keyFile := fs.String("key", "", "PKCS#8 DER Ed25519 private key file (local signing)")
	signCmd := fs.String("sign-cmd", "", "external custody signing command (hex hash in, hex signature out)")
	backend := fs.String("backend", "fake", "topology backend: fake | console")
	ledger := fs.String("ledger", "", "fake backend: shared ledger snapshot file")
	opsScript := fs.String("ops-script", "", "console backend: path to ops.canton")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *dir == "" || *actor == "" {
		return fmt.Errorf("resume: --dir and --actor are required")
	}

	st, spec, err := store.Open(*dir)
	if err != nil {
		return err
	}
	signer, err := buildSigner(*keyFile, *signCmd)
	if err != nil {
		return err
	}
	topo, err := buildTopology(*backend, *ledger, *opsScript, *actor)
	if err != nil {
		return err
	}
	flow, err := ceremony.NewOnboarding(spec, *actor, topo, signer, st)
	if err != nil {
		return err
	}
	status, err := flow.Advance(context.Background())
	if err != nil {
		return err
	}
	for _, key := range status.Ran {
		fmt.Println("ran:", key)
	}
	if status.Complete {
		fmt.Println("ceremony complete")
		return nil
	}
	fmt.Printf("waiting on %d ops (run resume as the responsible actors)\n", len(status.Waiting))
	return errWaiting
}

func buildSigner(keyFile, signCmd string) (ceremony.Signer, error) {
	switch {
	case keyFile != "" && signCmd != "":
		return nil, fmt.Errorf("resume: --key and --sign-cmd are mutually exclusive")
	case keyFile != "":
		return sign.NewKeySigner(keyFile)
	case signCmd != "":
		return sign.NewCmdSigner(signCmd)
	default:
		return nil, fmt.Errorf("resume: one of --key or --sign-cmd is required")
	}
}

func buildTopology(backend, ledger, opsScript, actor string) (ceremony.Topology, error) {
	switch backend {
	case "fake":
		if ledger == "" {
			return nil, fmt.Errorf("resume: --ledger is required for the fake backend")
		}
		return fake.NewFileTopology(ledger, "participant::"+actor), nil
	case "console":
		if opsScript == "" {
			return nil, fmt.Errorf("resume: --ops-script is required for the console backend")
		}
		return console.New(opsScript), nil
	default:
		return nil, fmt.Errorf("resume: unknown backend %q (want fake or console)", backend)
	}
}

func runStatus(args []string) error {
	fs := flag.NewFlagSet("status", flag.ContinueOnError)
	dir := fs.String("dir", "", "ceremony directory")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *dir == "" {
		return fmt.Errorf("status: --dir is required")
	}
	st, spec, err := store.Open(*dir)
	if err != nil {
		return err
	}
	keys, err := st.Keys()
	if err != nil {
		return err
	}
	sort.Strings(keys)
	fmt.Printf("ceremony %s: kind=%s owners=%d threshold=%d coordinator=%s\n",
		spec.WorkflowID, spec.Kind, len(spec.Owners), spec.Threshold, spec.Coordinator)
	for _, key := range keys {
		fmt.Println("done:", key)
	}
	var artifacts ceremony.Artifacts
	ok, err := st.Get("artifacts", &artifacts)
	if err != nil {
		return fmt.Errorf("status: reading artifacts: %w", err)
	}
	if ok {
		out, err := json.MarshalIndent(artifacts, "", "  ")
		if err != nil {
			return fmt.Errorf("status: formatting artifacts: %w", err)
		}
		fmt.Println("artifacts:", string(out))
	}
	return nil
}
