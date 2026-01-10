package calculator

import (
	"testing"
)

func TestAdd(t *testing.T) {
	result := Add(2, 3)
	if result != 5 {
		t.Errorf("Expected 5, got %d", result)
	}

	result = Add(-2, -3)
	if result != -5 {
		t.Errorf("Expected -5, got %d", result)
	}
}

func TestSubtract(t *testing.T) {
	result := Subtract(5, 3)
	if result != 2 {
		t.Errorf("Expected 2, got %d", result)
	}

	result = Subtract(-5, -3)
	if result != -2 {
		t.Errorf("Expected -2, got %d", result)
	}
}

func TestMultiply(t *testing.T) {
	result := Multiply(2, 3)
	if result != 6 {
		t.Errorf("Expected 6, got %d", result)
	}

	result = Multiply(-2, -3)
	if result != 6 {
		t.Errorf("Expected 6, got %d", result)
	}
}

func TestDivide(t *testing.T) {
	result := Divide(6, 3)
	if result != 2 {
		t.Errorf("Expected 2, got %d", result)
	}

	result = Divide(-6, -3)
	if result != 2 {
		t.Errorf("Expected 2, got %d", result)
	}

	_, err := Divide(1, 0)
	if err == nil {
		t.Error("Expected error when dividing by zero")
	}
}