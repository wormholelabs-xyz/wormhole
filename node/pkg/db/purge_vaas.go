package db

import (
	"fmt"
	"time"

	"github.com/dgraph-io/badger/v3"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

// This function deletes all VAAs for either the specified chain or specified chain / emitter address
// that are older than the specified time. If the logOnly flag is specified, it does not delete anything,
// just counts up what it would have deleted.

//nolint:unparam // Reports string return value as unused. Seems to be a false positive.
func (d *Database) PurgeVaas(prefix VAAID, oldestTime time.Time, logOnly bool) (string, error) {
	if prefix.Sequence != 0 {
		return "", fmt.Errorf("may not specify a sequence number on the prefix")
	}

	numDeleted := 0
	numKept := 0

	if err := d.db.View(func(txn *badger.Txn) error {
		it := txn.NewIterator(badger.DefaultIteratorOptions)
		defer it.Close()
		prefix := prefix.EmitterPrefixBytes()
		for it.Seek(prefix); it.ValidForPrefix(prefix); it.Next() {
			item := it.Item()
			key := item.Key()
			err := item.Value(func(val []byte) error {
				v, err := vaa.Unmarshal(val)
				if err != nil {
					return fmt.Errorf("failed to unmarshal VAA for %s: %v", string(key), err)
				}

				if v.Timestamp.Before(oldestTime) {
					numDeleted++
					if !logOnly {
						if err := d.db.Update(func(txn *badger.Txn) error {
							err := txn.Delete(key)
							return err
						}); err != nil {
							return fmt.Errorf("failed to delete vaa for key [%v]: %w", key, err)
						}
					}
				} else {
					numKept++
				}

				return nil
			})
			if err != nil {
				return err
			}
		}

		return nil
	}); err != nil {
		return "", err
	}

	ret := ""
	if logOnly {
		ret = fmt.Sprintf("Would purge VAAs for chain %s older than %v.\n", prefix.EmitterChain, oldestTime.String())
		if numDeleted != 0 {
			ret += fmt.Sprintf("Would have deleted %v items and kept %v.", numDeleted, numKept)
		} else {
			ret += fmt.Sprintf("Would not have deleted anything and kept %v items", numKept)
		}
	} else {
		ret = fmt.Sprintf("Purging VAAs for chain %s older than %v.\n", prefix.EmitterChain, oldestTime.String())
		if numDeleted != 0 {
			ret += fmt.Sprintf("Deleted %v items and kept %v items", numDeleted, numKept)
		} else {
			ret += fmt.Sprintf("Did not delete anything, kept %v items", numKept)
		}
	}

	return ret, nil
}

func (d *Database) PurgeSingleVaa(id VAAID, oldestTime time.Time, logOnly bool) (string, error) {
	key := id.Bytes()
	idStr := fmt.Sprintf("%d/%s/%d", id.EmitterChain, id.EmitterAddress, id.Sequence)

	var found bool
	var deleted bool
	if err := d.db.View(func(txn *badger.Txn) error {
		item, err := txn.Get(key)
		if err == badger.ErrKeyNotFound {
			return nil
		}
		if err != nil {
			return fmt.Errorf("failed to look up VAA for %s: %w", string(key), err)
		}

		found = true
		return item.Value(func(val []byte) error {
			v, err := vaa.Unmarshal(val)
			if err != nil {
				return fmt.Errorf("failed to unmarshal VAA for %s: %v", string(key), err)
			}

			if v.Timestamp.Before(oldestTime) {
				if !logOnly {
					if err := d.db.Update(func(txn *badger.Txn) error {
						return txn.Delete(key)
					}); err != nil {
						return fmt.Errorf("failed to delete vaa for key [%v]: %w", key, err)
					}
				}
				deleted = true
			}
			return nil
		})
	}); err != nil {
		return "", err
	}

	if !found {
		return fmt.Sprintf("VAA %s not found in database.", idStr), nil
	}
	if logOnly {
		if deleted {
			return fmt.Sprintf("Would delete VAA %s (older than %v).", idStr, oldestTime.String()), nil
		}
		return fmt.Sprintf("Would not delete VAA %s (not old enough, must be older than %v).", idStr, oldestTime.String()), nil
	}
	if deleted {
		return fmt.Sprintf("Deleted VAA %s.", idStr), nil
	}
	return fmt.Sprintf("Did not delete VAA %s (not old enough, must be older than %v).", idStr, oldestTime.String()), nil
}
