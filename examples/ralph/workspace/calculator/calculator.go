package calculator

import "errors"

// Add returns the sum of two float64 numbers
func Add(a, b float64) float64 {
    return a + b
}

// Subtract returns the difference of two float64 numbers
func Subtract(a, b float64) float64 {
    return a - b
}

// Multiply returns the product of two float64 numbers
func Multiply(a, b float64) float64 {
    return a * b
}

// Divide returns the quotient of two float64 numbers or an error if dividing by zero
func Divide(a, b float64) (float64, error) {
    if b == 0 {
        return 0, errors.New("division by zero")
    }
    return a / b, nil
}
