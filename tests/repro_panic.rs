use await_values::flip_card::FlipCard;
use std::sync::Arc;
use std::thread;

#[test]
fn reproduce_panic() {
    let card = Arc::new(FlipCard::new(0));
    let card_clone = card.clone();

    // Writer thread
    let writer = thread::spawn(move || {
        for i in 1..1000000 {
            card_clone.flip_to(i);
            // Small sleep to allow reader to interleave
            // thread::sleep(Duration::from_nanos(1));
        }
    });

    // Reader thread
    let reader = thread::spawn(move || {
        for _ in 0..1000000 {
            let _ = card.read();
        }
    });

    writer.join().unwrap();
    reader.join().unwrap();
}
