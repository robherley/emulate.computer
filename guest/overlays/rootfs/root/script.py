import platform

def fibonacci(count):
    a, b = 0, 1
    for _ in range(count):
        yield a
        a, b = b, a + b

print(f"Hello from Python on {platform.machine()}!")
print("The first 12 Fibonacci numbers:")
print(", ".join(str(number) for number in fibonacci(12)))
