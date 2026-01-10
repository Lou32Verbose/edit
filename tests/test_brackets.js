// JavaScript bracket test file

function simpleFunction(a, b) {
    return (a + b) * (a - b);
}

const nestedCallbacks = (x) => {
    return new Promise((resolve, reject) => {
        setTimeout(() => {
            if (x > 0) {
                resolve(x);
            } else {
                reject(new Error("negative"));
            }
        }, 1000);
    });
};

const arrayMethods = [1, 2, 3, 4, 5]
    .map((x) => x * 2)
    .filter((x) => x > 4)
    .reduce((acc, x) => acc + x, 0);

const destructuring = ({ a, b: { c, d: [e, f] } }) => {
    return [a, c, e, f];
};

const objectLiteral = {
    method() {
        return {
            nested: {
                array: [
                    { item: 1 },
                    { item: 2 }
                ]
            }
        };
    }
};

// Template literals with expressions
const template = `Result: ${(a + b) * [1, 2, 3].length}`;

// Arrow functions
const arrows = [
    () => {},
    (x) => x,
    (x, y) => ({ x, y }),
    ([a, b]) => [b, a],
];

// JSX-like (for React testing)
const jsx = (
    <div>
        <span>{items.map((i) => (<li key={i}>{i}</li>))}</span>
    </div>
);
