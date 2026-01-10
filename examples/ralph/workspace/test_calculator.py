import pytest
from calculator import add, subtract, multiply, divide


def test_add():
    assert add(1, 2) == 3
    assert add(-1, -1) == -2
    assert add(-1, 1) == 0


def test_subtract():
    assert subtract(2, 1) == 1
    assert subtract(-1, -1) == 0
    assert subtract(1, -1) == 2


def test_multiply():
    assert multiply(2, 3) == 6
    assert multiply(-1, -1) == 1
    assert multiply(-1, 1) == -1


def test_divide():
    assert divide(4, 2) == 2
    assert divide(-1, -1) == 1
    assert divide(-1, 1) == -1
    
    with pytest.raises(ZeroDivisionError):
        divide(1, 0)