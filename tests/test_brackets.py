# Python bracket test file

def simple_function(a, b):
    return (a + b) * (a - b)

def nested_structures():
    nested_list = [
        [1, 2, [3, 4]],
        [5, [6, [7, 8]]],
    ]

    nested_dict = {
        "level1": {
            "level2": {
                "level3": ["a", "b", "c"]
            }
        }
    }

    return (nested_list, nested_dict)

def list_comprehensions():
    # Simple
    squares = [x**2 for x in range(10)]

    # Nested
    matrix = [[i*j for j in range(5)] for i in range(5)]

    # With condition
    evens = [x for x in range(20) if (x % 2) == 0]

    # Dict comprehension
    square_dict = {x: (x**2) for x in range(10)}

    return (squares, matrix, evens, square_dict)

def generator_expressions():
    gen = (x**2 for x in range(10) if (x % 2) == 0)
    return list(gen)

def tuple_operations():
    # Nested tuples
    nested = ((1, 2), (3, (4, 5)), ((6, 7), 8))

    # Tuple unpacking
    (a, (b, c)), d = ((1, (2, 3)), 4)

    return (a, b, c, d)

def function_calls():
    result = some_function(
        arg1,
        nested_call(
            inner_arg1,
            inner_arg2
        ),
        [1, 2, 3],
        {"key": "value"}
    )
    return result

class BracketClass:
    def __init__(self, data: dict[str, list[int]]):
        self.data = data

    def method(self) -> tuple[int, list[str]]:
        return (1, ["a", "b"])

# Lambda with brackets
process = lambda x: (x[0], x[1]) if len(x) > 1 else (x[0], None)

# Edge cases
empty_structures = ((), [], {})
# adjacent = ()[]{}  # Syntax error in Python, but valid bracket test visually
