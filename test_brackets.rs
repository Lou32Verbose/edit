// Test file for bracket highlighting
// Place cursor on any bracket to see matching bracket highlighted

fn simple_parens() {
    let x = (1 + 2);
    let y = ((3 + 4) * 5);
}

fn nested_brackets() {
    let arr = [1, [2, [3, 4], 5], 6];
    let map = {
        "key": {
            "nested": [1, 2, 3]
        }
    };
}

fn mixed_brackets() {
    if (condition) {
        match value {
            Some(x) => vec![x],
            None => vec![],
        }
    }
}

fn deep_nesting() {
    ((((((((((deep))))))))))
    [[[[[[[[[[deep]]]]]]]]]]
    {{{{{{{{{{deep}}}}}}}}}}
}

fn function_calls(arg1: (i32, i32), arg2: [u8; 4]) -> Result<Vec<String>, Error> {
    let tuple = (1, (2, (3, (4, 5))));
    let array = [1, [2, 3], [[4, 5], [6, 7]]];
    Ok(vec!["test".to_string()])
}

// Edge cases
fn edge_cases() {
    // Empty brackets
    let empty_parens = ();
    let empty_array: [i32; 0] = [];
    let empty_block = {};

    // Brackets in strings (should not match)
    let string_with_brackets = "( [ { } ] )";

    // Adjacent brackets
    let adjacent = ()[]{}()[];

    // Brackets on same line
    (a)(b)(c)[d][e]{f}{g}
}

// Multiline brackets
fn multiline() {
    let result = some_function(
        arg1,
        arg2,
        arg3
    );

    let vec = vec![
        item1,
        item2,
        item3,
    ];

    if condition {
        // block
        // content
        // here
    }
}
