use rayon::iter::{
    IntoParallelIterator as RayonIntoParallelIterator, ParallelIterator as RayonParallelIterator,
};
use std::{
    hint::black_box,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use weave::iter::{
    IntoParallelIterator as WeaveIntoParallelIterator, ParallelIterator as WeaveParallelIterator,
};

#[cfg(feature = "allocation-metrics")]
mod allocations {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    pub static ACTIVE: AtomicBool = AtomicBool::new(false);
    pub static COUNT: AtomicU64 = AtomicU64::new(0);
    pub static BYTES: AtomicU64 = AtomicU64::new(0);
    pub struct CountAlloc;
    fn record(size: usize) {
        if ACTIVE.load(Ordering::Relaxed) {
            COUNT.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(size as u64, Ordering::Relaxed);
        }
    }
    // SAFETY: every allocation operation is forwarded unchanged to System;
    // only nonallocating atomic counters are updated. Realloc's new requested
    // size is counted, not net live memory or actual physical allocation.
    unsafe impl GlobalAlloc for CountAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                record(layout.size());
            }
            ptr
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            let result = unsafe { System.realloc(ptr, layout, size) };
            if !result.is_null() {
                record(size);
            }
            result
        }
    }
}
#[cfg(feature = "allocation-metrics")]
#[global_allocator]
static ALLOCATOR: allocations::CountAlloc = allocations::CountAlloc;

struct Engines {
    weave: weave::ThreadPool,
    rayon: rayon::ThreadPool,
    data: Vec<u64>,
}
fn work(mut n: u64, cost: usize) -> u64 {
    n = black_box(n);
    for _ in 0..cost {
        n ^= n >> 12;
        n ^= n << 25;
        n ^= n >> 27;
        n = n.wrapping_mul(2685821657736338717);
    }
    n
}
fn join_weave(pool: &weave::ThreadPool, depth: usize) -> u64 {
    if depth == 0 {
        return work(7, 32);
    }
    let (a, b) = pool.join(
        || join_weave(pool, depth - 1),
        || join_weave(pool, depth - 1),
    );
    a.wrapping_add(b)
}
fn join_sequential(depth: usize) -> u64 {
    if depth == 0 {
        return work(7, 32);
    }
    join_sequential(depth - 1).wrapping_add(join_sequential(depth - 1))
}
fn join_rayon(depth: usize) -> u64 {
    if depth == 0 {
        return work(7, 32);
    }
    let (a, b) = rayon::join(|| join_rayon(depth - 1), || join_rayon(depth - 1));
    a.wrapping_add(b)
}
fn run(engines: &Engines, engine: &str, scenario: &str, n: usize) -> u64 {
    let filtered = scenario == "filter_reduce";
    let cost = match scenario {
        "map_heavy" => 128,
        "map_medium" => 16,
        _ => 1,
    };
    match scenario {
        "join" | "nested" => {
            let depth = if scenario == "join" { 7 } else { 9 };
            match engine {
                "weave" => engines.weave.install(|| join_weave(&engines.weave, depth)),
                "rayon" => engines.rayon.install(|| join_rayon(depth)),
                _ => join_sequential(depth),
            }
        }
        "spawn_empty" | "submit_small" | "scope" | "external_producers" | "heterogeneous"
        | "priority_labels" => {
            let total = Arc::new(AtomicU64::new(0));
            let task_cost = if scenario == "spawn_empty" { 0 } else { 16 };
            let calc = |i: usize| {
                work(
                    i as u64,
                    if scenario == "heterogeneous" {
                        1 + i % 127
                    } else {
                        task_cost
                    },
                )
            };
            let producers = if scenario == "external_producers" {
                4
            } else {
                1
            };
            match engine {
                "weave" => {
                    let publish = |producer| {
                        if scenario == "submit_small" {
                            let handles: Vec<_> = (producer..n)
                                .step_by(producers)
                                .map(|i| engines.weave.submit(move || work(i as u64, 16)))
                                .collect();
                            for h in handles {
                                total.fetch_add(h.join().unwrap(), Ordering::Relaxed);
                            }
                        } else if matches!(scenario, "spawn_empty" | "priority_labels") {
                            let (tx, rx) = std::sync::mpsc::channel();
                            for i in (producer..n).step_by(producers) {
                                let (total, tx) = (total.clone(), tx.clone());
                                let priority = if scenario == "priority_labels" {
                                    match i % 3 {
                                        0 => weave::Priority::High,
                                        1 => weave::Priority::Normal,
                                        _ => weave::Priority::Low,
                                    }
                                } else {
                                    weave::Priority::Normal
                                };
                                engines.weave.spawn(
                                    weave::Job::new(move || {
                                        total.fetch_add(
                                            work(i as u64, task_cost),
                                            Ordering::Relaxed,
                                        );
                                        tx.send(()).unwrap();
                                    })
                                    .set_priority(priority)
                                    .set_label("baseline"),
                                );
                            }
                            drop(tx);
                            for _ in rx {}
                        } else {
                            engines.weave.scope(|scope| {
                                for i in (producer..n).step_by(producers) {
                                    let (total, calc) = (&total, &calc);
                                    scope.spawn(move || {
                                        total.fetch_add(calc(i), Ordering::Relaxed);
                                    });
                                }
                            });
                        }
                    };
                    if producers == 1 {
                        publish(0);
                    } else {
                        std::thread::scope(|threads| {
                            for producer in 0..producers {
                                threads.spawn(move || publish(producer));
                            }
                        });
                    }
                }
                "rayon" => {
                    let publish = |producer| {
                        engines.rayon.scope(|scope| {
                            for i in (producer..n).step_by(producers) {
                                let (total, calc) = (&total, &calc);
                                scope.spawn(move |_| {
                                    total.fetch_add(calc(i), Ordering::Relaxed);
                                });
                            }
                        })
                    };
                    if producers == 1 {
                        publish(0);
                    } else {
                        std::thread::scope(|threads| {
                            for producer in 0..producers {
                                threads.spawn(move || publish(producer));
                            }
                        });
                    }
                }
                _ => {
                    for i in 0..n {
                        total.fetch_add(calc(i), Ordering::Relaxed);
                    }
                }
            }
            total.load(Ordering::Relaxed)
        }

        "collect" => {
            let values: Vec<u64> = match engine {
                "weave" => engines
                    .weave
                    .install(|| (0..n).parallelize().map(|i| work(i as u64, 1)).collect()),
                "rayon" => engines
                    .rayon
                    .install(|| (0..n).into_par_iter().map(|i| work(i as u64, 1)).collect()),
                _ => (0..n).map(|i| work(i as u64, 1)).collect(),
            };
            black_box(values).into_iter().fold(0u64, u64::wrapping_add)
        }
        "memory_cache" | "memory_contiguous" | "memory_scattered" => {
            let input = &engines.data[..n];
            let index = |i| {
                if scenario == "memory_scattered" {
                    (i * 8191) % n
                } else {
                    i
                }
            };
            let calc = |i| black_box(input[index(i)]);
            match engine {
                "weave" => engines.weave.install(|| {
                    (0..n)
                        .parallelize()
                        .map(calc)
                        .reduce(u64::wrapping_add)
                        .unwrap()
                }),
                "rayon" => engines.rayon.install(|| {
                    (0..n)
                        .into_par_iter()
                        .map(calc)
                        .reduce(|| 0, u64::wrapping_add)
                }),
                _ => (0..n).map(calc).fold(0, u64::wrapping_add),
            }
        }
        _ => match engine {
            "weave" => engines.weave.install(|| {
                (0..n)
                    .parallelize()
                    .filter(|i| !filtered || i % 3 == 0)
                    .map(|i| work(i as u64, cost))
                    .reduce(u64::wrapping_add)
                    .unwrap_or(0)
            }),
            "rayon" => engines.rayon.install(|| {
                (0..n)
                    .into_par_iter()
                    .filter(|i| !filtered || i % 3 == 0)
                    .map(|i| work(i as u64, cost))
                    .reduce(|| 0, u64::wrapping_add)
            }),
            _ => (0..n)
                .filter(|i| !filtered || i % 3 == 0)
                .map(|i| work(i as u64, cost))
                .fold(0, u64::wrapping_add),
        },
    }
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("time");
    let workers: usize = args.get(2).unwrap_or(&"1".into()).parse().unwrap();
    let samples: usize = std::env::var("WEAVE_BENCH_SAMPLES")
        .ok()
        .map(|s| s.parse().unwrap())
        .unwrap_or(9);
    // Pool lifecycle is measured separately, never inside a steady-state row.
    if mode == "time" {
        println!("mode,engine,workers,scenario,n,sample,elapsed_ns,checksum");
        for engine in ["weave", "rayon"] {
            for sample in 0..samples {
                let start = Instant::now();
                if engine == "weave" {
                    drop(weave::ThreadPoolBuilder::new().num_threads(workers).build());
                } else {
                    drop(
                        rayon::ThreadPoolBuilder::new()
                            .num_threads(workers)
                            .build()
                            .unwrap(),
                    );
                }
                println!(
                    "time,{engine},{workers},pool_lifecycle,0,{sample},{},0",
                    start.elapsed().as_nanos()
                );
            }
        }
    } else if mode == "allocations" {
        println!("mode,engine,workers,scenario,n,sample,allocation_calls,requested_bytes,checksum");
        #[cfg(not(feature = "allocation-metrics"))]
        panic!("allocations mode requires allocation-metrics");
    } else if mode == "contention" {
        println!("mode,engine,workers,scenario,n,sample,lock_attempts,contended,wait_ns,checksum");
        #[cfg(not(feature = "scheduler-metrics"))]
        panic!("contention mode requires scheduler-metrics");
    } else if mode == "latency" {
        println!("mode,engine,workers,scenario,n,sample,dispatch_ns,completion_ns");
    } else {
        panic!("unknown mode");
    }

    let engines = Engines {
        weave: weave::ThreadPoolBuilder::new().num_threads(workers).build(),
        data: (0..4_194_304).map(|i| i as u64).collect(),
        rayon: rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .unwrap(),
    };
    if mode == "latency" {
        for engine in ["weave", "rayon", "sequential"] {
            for scenario in ["burst_small", "priority_mix"] {
                if scenario == "priority_mix" && engine != "weave" {
                    continue;
                }
                for pass in 0..samples + 2 {
                    let latencies = Arc::new(std::sync::Mutex::new(Vec::new()));
                    let capture = |submitted: Instant, priority: usize| {
                        let started = submitted.elapsed().as_nanos();
                        black_box(work(7, 32));
                        (priority, started, submitted.elapsed().as_nanos())
                    };
                    match engine {
                        "weave" => engines.weave.scope(|s| {
                            for i in 0..256 {
                                let submitted = Instant::now();
                                let latencies = latencies.clone();
                                let rank = if scenario == "priority_mix" { i % 3 } else { 1 };
                                let priority = match rank {
                                    0 => weave::Priority::High,
                                    1 => weave::Priority::Normal,
                                    _ => weave::Priority::Low,
                                };
                                s.spawn_with_priority(priority, move || {
                                    let measurement = capture(submitted, rank);
                                    latencies.lock().unwrap().push(measurement);
                                });
                            }
                        }),
                        "rayon" => engines.rayon.scope(|s| {
                            for _ in 0..256 {
                                let submitted = Instant::now();
                                let latencies = latencies.clone();
                                s.spawn(move |_| {
                                    let measurement = capture(submitted, 1);
                                    latencies.lock().unwrap().push(measurement);
                                });
                            }
                        }),
                        _ => {
                            for _ in 0..256 {
                                let submitted = Instant::now();
                                let measurement = capture(submitted, 1);
                                latencies.lock().unwrap().push(measurement);
                            }
                        }
                    }
                    if pass < 2 {
                        continue;
                    }
                    for (index, (rank, dispatch, completion)) in
                        latencies.lock().unwrap().iter().enumerate()
                    {
                        let label = if scenario == "priority_mix" {
                            match rank {
                                0 => "priority_mix_high",
                                1 => "priority_mix_normal",
                                _ => "priority_mix_low",
                            }
                        } else {
                            "burst_small"
                        };
                        let sample = (pass - 2) * 256 + index;
                        println!(
                            "latency,{engine},{workers},{label},256,{sample},{dispatch},{completion}"
                        );
                    }
                }
            }
        }
        return;
    }

    for (scenario, n) in [
        ("spawn_empty", 256),
        ("submit_small", 256),
        ("scope", 256),
        ("external_producers", 1024),
        ("heterogeneous", 1024),
        ("priority_labels", 1024),
        ("join", 128),
        ("nested", 512),
        ("map_small", 128),
        ("map_light", 1_048_576),
        ("map_medium", 262_144),
        ("map_heavy", 16_384),
        ("filter_reduce", 262_144),
        ("collect", 65_536),
        ("memory_cache", 32_768),
        ("memory_contiguous", 4_194_304),
        ("memory_scattered", 4_194_304),
    ] {
        let expected = run(&engines, "sequential", scenario, n);
        for engine in ["weave", "rayon", "sequential"] {
            if mode == "contention" && engine != "weave" {
                continue;
            }
            for _ in 0..2 {
                assert_eq!(black_box(run(&engines, engine, scenario, n)), expected);
            }
            for sample in 0..samples {
                if mode == "time" {
                    let start = Instant::now();
                    let value = black_box(run(&engines, engine, scenario, n));
                    let elapsed = start.elapsed().as_nanos();
                    assert_eq!(value, expected);
                    println!("time,{engine},{workers},{scenario},{n},{sample},{elapsed},{value}");
                }
                #[cfg(feature = "allocation-metrics")]
                if mode == "allocations" {
                    allocations::COUNT.store(0, Ordering::SeqCst);
                    allocations::BYTES.store(0, Ordering::SeqCst);
                    allocations::ACTIVE.store(true, Ordering::SeqCst);
                    let value = black_box(run(&engines, engine, scenario, n));
                    allocations::ACTIVE.store(false, Ordering::SeqCst);
                    assert_eq!(value, expected);
                    println!(
                        "allocations,{engine},{workers},{scenario},{n},{sample},{},{},{value}",
                        allocations::COUNT.load(Ordering::SeqCst),
                        allocations::BYTES.load(Ordering::SeqCst)
                    );
                }
                #[cfg(feature = "scheduler-metrics")]
                if mode == "contention" {
                    let before = engines.weave.scheduler_metrics();
                    let value = black_box(run(&engines, engine, scenario, n));
                    let after = engines.weave.scheduler_metrics();
                    assert_eq!(value, expected);
                    println!(
                        "contention,{engine},{workers},{scenario},{n},{sample},{},{},{},{value}",
                        after.lock_attempts - before.lock_attempts,
                        after.contended - before.contended,
                        after.wait_ns - before.wait_ns
                    );
                }
            }
        }
    }
}
