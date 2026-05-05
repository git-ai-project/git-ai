use super::transcript_worker::{Priority, ProcessingTask};
use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[test]
fn test_priority_queue_ordering_immediate_first() {
    let mut heap = BinaryHeap::new();

    // Insert tasks in reverse priority order
    heap.push(ProcessingTask {
        session_id: "low".to_string(),
        priority: Priority::Low,
        agent_type: "test".to_string(),
        canonical_path: PathBuf::from("/test"),
        retry_count: 0,
        next_retry_at: None,
    });
    heap.push(ProcessingTask {
        session_id: "immediate".to_string(),
        priority: Priority::Immediate,
        agent_type: "test".to_string(),
        canonical_path: PathBuf::from("/test"),
        retry_count: 0,
        next_retry_at: None,
    });

    // pop() should return Immediate first, then High, then Low
    let first = heap.pop().unwrap();
    assert_eq!(
        first.priority,
        Priority::Immediate,
        "Immediate priority should be popped first"
    );
    assert_eq!(first.session_id, "immediate");

    let second = heap.pop().unwrap();
    assert_eq!(
        second.priority,
        Priority::Low,
        "Low priority should be popped last"
    );
    assert_eq!(second.session_id, "low");
}

#[test]
fn test_priority_queue_ordering_multiple_same_priority() {
    let mut heap = BinaryHeap::new();

    heap.push(ProcessingTask {
        session_id: "immediate-2".to_string(),
        priority: Priority::Immediate,
        agent_type: "test".to_string(),
        canonical_path: PathBuf::from("/test"),
        retry_count: 0,
        next_retry_at: None,
    });
    heap.push(ProcessingTask {
        session_id: "low-1".to_string(),
        priority: Priority::Low,
        agent_type: "test".to_string(),
        canonical_path: PathBuf::from("/test"),
        retry_count: 0,
        next_retry_at: None,
    });
    heap.push(ProcessingTask {
        session_id: "immediate-1".to_string(),
        priority: Priority::Immediate,
        agent_type: "test".to_string(),
        canonical_path: PathBuf::from("/test"),
        retry_count: 0,
        next_retry_at: None,
    });

    // Both immediate tasks should come out before low
    let first = heap.pop().unwrap();
    assert_eq!(first.priority, Priority::Immediate);

    let second = heap.pop().unwrap();
    assert_eq!(second.priority, Priority::Immediate);

    let third = heap.pop().unwrap();
    assert_eq!(third.priority, Priority::Low);
}

#[test]
fn test_retry_delay_prevents_immediate_reprocessing() {
    let mut heap = BinaryHeap::new();

    // Create a task with retry scheduled for 5 seconds in the future
    let now = Instant::now();
    let next_retry_at = now + Duration::from_secs(5);

    let task = ProcessingTask {
        session_id: "retry-test".to_string(),
        priority: Priority::Immediate,
        agent_type: "test".to_string(),
        canonical_path: PathBuf::from("/test"),
        retry_count: 1,
        next_retry_at: Some(next_retry_at),
    };

    heap.push(task.clone());

    // Task should pop from heap
    let popped = heap.pop().unwrap();
    assert_eq!(popped.session_id, "retry-test");

    // But it should NOT be processable until next_retry_at has passed
    assert!(popped.next_retry_at.is_some());
    assert!(
        popped.next_retry_at.unwrap() > now,
        "Task should have a future retry time"
    );

    // Simulating the check: is it time to process?
    let ready_to_process = popped
        .next_retry_at
        .map(|retry_at| Instant::now() >= retry_at)
        .unwrap_or(true);

    assert!(
        !ready_to_process,
        "Task should not be ready for immediate processing"
    );
}

#[test]
fn test_retry_delay_allows_processing_after_delay() {
    let now = Instant::now();

    // Create a task with retry scheduled for the past (simulating time has passed)
    let past_retry_at = now - Duration::from_secs(1);

    let task = ProcessingTask {
        session_id: "retry-past".to_string(),
        priority: Priority::Immediate,
        agent_type: "test".to_string(),
        canonical_path: PathBuf::from("/test"),
        retry_count: 1,
        next_retry_at: Some(past_retry_at),
    };

    // Check if ready to process
    let ready_to_process = task
        .next_retry_at
        .map(|retry_at| Instant::now() >= retry_at)
        .unwrap_or(true);

    assert!(
        ready_to_process,
        "Task with past retry time should be ready for processing"
    );
}
