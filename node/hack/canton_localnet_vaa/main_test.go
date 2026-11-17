package main

import (
	"reflect"
	"testing"
)

func TestParseSignerPositions(t *testing.T) {
	testCases := []struct {
		name              string
		flagValue         string
		setSize           int
		expectedPositions []int
		expectError       bool
	}{
		{name: "empty selects all", flagValue: "", setSize: 3, expectedPositions: []int{0, 1, 2}},
		{name: "subset", flagValue: "0,1", setSize: 3, expectedPositions: []int{0, 1}},
		{name: "last position", flagValue: "2", setSize: 3, expectedPositions: []int{2}},
		{name: "descending", flagValue: "1,0", setSize: 3, expectError: true},
		{name: "duplicate", flagValue: "1,1", setSize: 3, expectError: true},
		{name: "equal to set size", flagValue: "3", setSize: 3, expectError: true},
		{name: "negative", flagValue: "-1", setSize: 3, expectError: true},
		{name: "not a number", flagValue: "a", setSize: 3, expectError: true},
		{name: "empty field", flagValue: "0,", setSize: 3, expectError: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			positions, err := parseSignerPositions(testCase.flagValue, testCase.setSize)
			if testCase.expectError {
				if err == nil {
					t.Fatalf("expected an error, got positions %v", positions)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if !reflect.DeepEqual(positions, testCase.expectedPositions) {
				t.Fatalf("positions %v, expected %v", positions, testCase.expectedPositions)
			}
		})
	}
}
