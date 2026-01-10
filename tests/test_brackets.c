// C bracket test file

#include <stdio.h>

int simple_function(int a, int b) {
    return (a + b) * (a - b);
}

void nested_blocks() {
    if (condition) {
        if (another_condition) {
            while (loop_condition) {
                for (int i = 0; i < 10; i++) {
                    switch (value) {
                        case 1: {
                            // nested block in switch
                            break;
                        }
                        default: {
                            break;
                        }
                    }
                }
            }
        }
    }
}

void array_operations() {
    int arr[10] = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9};
    int matrix[3][3] = {
        {1, 2, 3},
        {4, 5, 6},
        {7, 8, 9}
    };

    // Array access
    int val = arr[matrix[0][1]];

    // Nested array access
    int deep = matrix[arr[0]][arr[1]];
}

void pointer_and_cast() {
    int x = 10;
    int *p = &x;

    // Cast expressions
    float f = (float)(x * 2);
    void *vp = (void *)((char *)p + 1);

    // Function pointer
    int (*func_ptr)(int, int) = &simple_function;
    int result = (*func_ptr)(1, 2);
}

struct nested_struct {
    struct {
        int inner_array[5];
        struct {
            int deep_value;
        } deep;
    } nested;
};

// Macro with brackets
#define MAX(a, b) ((a) > (b) ? (a) : (b))
#define ARRAY_SIZE(arr) (sizeof(arr) / sizeof((arr)[0]))

int main(int argc, char *argv[]) {
    // Expression with many brackets
    int result = ((((1 + 2) * 3) - 4) / 5);

    // Compound literal
    struct nested_struct s = {
        .nested = {
            .inner_array = {1, 2, 3, 4, 5},
            .deep = {.deep_value = 42}
        }
    };

    return 0;
}
