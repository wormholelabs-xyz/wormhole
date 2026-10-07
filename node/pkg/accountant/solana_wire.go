// Field types and codecs for the generated wire structs in solana_layout_gen.go.

package accountant

import (
	"encoding/binary"
	"fmt"
)

// wireLayout is a generated struct with the byte layout of one svm/accountant layout.
type wireLayout interface {
	wireLen() int
	wireName() string
}

// be16 is a big-endian uint16 field.
type be16 [2]byte

func newBE16(v uint16) be16 {
	var b be16
	binary.BigEndian.PutUint16(b[:], v)
	return b
}

func (b be16) Uint16() uint16 { return binary.BigEndian.Uint16(b[:]) }

func (b be16) bytes() []byte { return b[:] }

// be32 is a big-endian uint32. PDA seeds use it.
type be32 [4]byte

func newBE32(v uint32) be32 {
	var b be32
	binary.BigEndian.PutUint32(b[:], v)
	return b
}

func (b be32) bytes() []byte { return b[:] }

// be64 is a big-endian uint64 field.
type be64 [8]byte

func newBE64(v uint64) be64 {
	var b be64
	binary.BigEndian.PutUint64(b[:], v)
	return b
}

func (b be64) Uint64() uint64 { return binary.BigEndian.Uint64(b[:]) }

func (b be64) bytes() []byte { return b[:] }

// decodeWire decodes exactly out.wireLen() bytes. Plain integer fields are little-endian.
func decodeWire[T wireLayout](data []byte, out *T) error {
	name, wantLen := (*out).wireName(), (*out).wireLen()
	if len(data) != wantLen {
		return fmt.Errorf("%s: want %d bytes, got %d", name, wantLen, len(data))
	}
	n, err := binary.Decode(data, binary.LittleEndian, out)
	if err != nil {
		return fmt.Errorf("%s: %w", name, err)
	}
	if n != wantLen {
		return fmt.Errorf("%s: decoded %d bytes, want %d", name, n, wantLen)
	}
	return nil
}

// encodeWire encodes v into exactly v.wireLen() bytes. Blank padding fields are zero.
func encodeWire[T wireLayout](v *T) ([]byte, error) {
	name, wantLen := (*v).wireName(), (*v).wireLen()
	out := make([]byte, wantLen)
	n, err := binary.Encode(out, binary.LittleEndian, v)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", name, err)
	}
	if n != wantLen {
		return nil, fmt.Errorf("%s: encoded %d bytes, want %d", name, n, wantLen)
	}
	return out, nil
}
