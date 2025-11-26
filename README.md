# await_values

Primitives for subscribing to / notifying about changes to values.

![logo](art/logo.png)

This library provides a simple way to create observable values that can notify multiple
observers when they change. It's particularly useful for GUI applications, state management,
and reactive programming patterns.

## Core Concepts

This library primarily imagines your value type is:
* `Clone` - so that it can be cloned for observers.
* `PartialEq` - so that we can diff values and only notify observers when the value changes.

Our cast of characters includes:
* `Value` - Allocates storage for a value that can be observed.
* `Observer` - A handle to a value that can be used to observe when the value changes remotely.
* `aggregate::AggregateObserver` - A handle to multiple heterogeneous values that can be used to observe when any of the values change.

This library uses asynchronous functions and is executor-agnostic. It does not depend on tokio.

The library uses lock-free atomic algorithms for high-performance concurrent access.

## Quick Start

```rust
use await_values::{Value, Observer};

// Create an observable value
let value = Value::new(42);

// Create an observer
let mut observer = value.observe();

// Get the current value
assert_eq!(observer.next().await.unwrap(), 42);

// Update the value
value.set(100);

// Observe the change
assert_eq!(observer.next().await.unwrap(), 100);
```

## Advanced Usage

### Observing Multiple Values

You can observe multiple values of different types using `AggregateObserver`:

```rust
use await_values::{Value, aggregate::AggregateObserver};

let temperature = Value::new(20.5);
let status = Value::new("OK");

let mut aggregate = AggregateObserver::new();
aggregate.add_observer(temperature.observe());
aggregate.add_observer(status.observe());

// Wait for initial values
let index = aggregate.next().await;
assert!(index == 0 || index == 1);

// Change a value
temperature.set(25.0);

// See which observer changed
let changed_index = aggregate.next().await;
assert_eq!(changed_index, 0); // temperature changed
```
## Thread Safety

All types in this library are thread-safe and can be shared across threads.
`Value` uses interior mutability with proper synchronization, making it safe to use from multiple threads.

```rust
use await_values::Value;
use std::sync::Arc;
use std::thread;

// Wrap Value in Arc to share between threads
let value = Arc::new(Value::new(0));
let value_clone = Arc::clone(&value);

let handle = thread::spawn(move || {
    value_clone.set(42);
});

handle.join().unwrap();
assert_eq!(value.get(), 42);
```