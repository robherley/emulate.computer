function* fibonacci(count) {
  let a = 0;
  let b = 1;
  for (let i = 0; i < count; i++) {
    yield a;
    [a, b] = [b, a + b];
  }
}

console.log("Hello from JavaScript on QuickJS!");
console.log("The first 12 Fibonacci numbers:");
console.log([...fibonacci(12)].join(", "));
