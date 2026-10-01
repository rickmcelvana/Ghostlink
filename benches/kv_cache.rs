use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use ghostlink_core::kv_cache::{KVCacheConfig, LayerKvCache};

fn bench_kv_cache(c: &mut Criterion) {
    let configs = [
        (
            "small",
            KVCacheConfig {
                max_tokens_per_seq: 1024,
                num_heads: 4,
                hidden_dim_per_head: 64,
            },
        ),
        (
            "default",
            KVCacheConfig {
                max_tokens_per_seq: 8192,
                num_heads: 4,
                hidden_dim_per_head: 64,
            },
        ),
        (
            "wide",
            KVCacheConfig {
                max_tokens_per_seq: 2048,
                num_heads: 32,
                hidden_dim_per_head: 128,
            },
        ),
    ];

    for &(config_name, config) in &configs {
        let mut group = c.benchmark_group("kv_cache");
        if config_name == "wide" {
            group.sample_size(10);
            group.measurement_time(Duration::from_secs(1));
            group.warm_up_time(Duration::from_millis(500));
        } else {
            group.sample_size(50);
            group.measurement_time(Duration::from_secs(2));
            group.warm_up_time(Duration::from_secs(1));
        }

        let width = config.token_width();
        let keys = vec![1.0f32; width];
        let values = vec![2.0f32; width];

        // 1. initialize
        group.bench_function(BenchmarkId::new("initialize", config_name), |b| {
            b.iter_batched(
                || LayerKvCache::new(config),
                |cache| {
                    cache.initialize(config.max_tokens_per_seq).unwrap();
                    cache
                },
                BatchSize::SmallInput,
            );
        });

        // 2. write_kv sequential fill 0..256
        group.bench_function(BenchmarkId::new("write_kv_fill_256", config_name), |b| {
            b.iter_batched(
                || {
                    let cache = LayerKvCache::new(config);
                    cache.initialize(256).unwrap();
                    cache
                },
                |cache| {
                    for t in 0..256 {
                        cache.write_kv(t, &keys, &values).unwrap();
                    }
                },
                BatchSize::SmallInput,
            );
        });

        // 3. write_kv_batch 64 tokens
        let batch_entries: Vec<(usize, &[f32], &[f32])> = (0..64)
            .map(|i| (i, keys.as_slice(), values.as_slice()))
            .collect();
        group.bench_function(BenchmarkId::new("write_kv_batch_64", config_name), |b| {
            b.iter_batched(
                || {
                    let cache = LayerKvCache::new(config);
                    cache.initialize(64).unwrap();
                    cache
                },
                |cache| {
                    cache.write_kv_batch(&batch_entries).unwrap();
                },
                BatchSize::SmallInput,
            );
        });

        // Setup a prefilled cache with 1024 tokens for read benchmarks
        let prefilled = LayerKvCache::new(config);
        prefilled.initialize(1024).unwrap();
        for t in 0..1024 {
            prefilled.write_kv(t, &keys, &values).unwrap();
        }

        // 4. read_kv mid-sequence (owned path)
        group.bench_function(BenchmarkId::new("read_kv_mid", config_name), |b| {
            b.iter(|| {
                black_box(prefilled.read_kv(black_box(128)).unwrap());
            });
        });

        // 4b. with_read_kv mid-sequence (zero-copy closure path)
        group.bench_function(
            BenchmarkId::new("with_read_kv_mid_zero_copy", config_name),
            |b| {
                b.iter(|| {
                    prefilled
                        .with_read_kv(black_box(128), |k, v| {
                            black_box(k[0] + v[0]);
                        })
                        .unwrap();
                });
            },
        );

        // 5. read_range 1 token, 64 tokens, 1024 tokens (owned path + zero-copy paths)
        for &span in &[1usize, 64, 1024] {
            group.bench_function(
                BenchmarkId::new(format!("read_range_{span}"), config_name),
                |b| {
                    b.iter(|| {
                        black_box(prefilled.read_range(black_box(0), black_box(span)).unwrap());
                    });
                },
            );

            group.bench_function(
                BenchmarkId::new(format!("with_read_range_{span}_zero_copy"), config_name),
                |b| {
                    b.iter(|| {
                        prefilled
                            .with_read_range(black_box(0), black_box(span), |s| {
                                black_box(s.keys.len());
                            })
                            .unwrap();
                    });
                },
            );

            let mut keys_buf = vec![0.0f32; span * width];
            let mut values_buf = vec![0.0f32; span * width];
            group.bench_function(
                BenchmarkId::new(format!("read_range_into_{span}_caller_buf"), config_name),
                |b| {
                    b.iter(|| {
                        prefilled
                            .read_range_into(
                                black_box(0),
                                black_box(span),
                                black_box(&mut keys_buf),
                                black_box(&mut values_buf),
                            )
                            .unwrap();
                    });
                },
            );
        }

        // 6. decode_step: write_kv(t) + read_range(0, t+1) for t in a 256-step loop
        group.bench_function(BenchmarkId::new("decode_step_256", config_name), |b| {
            b.iter_batched(
                || {
                    let cache = LayerKvCache::new(config);
                    cache.initialize(256).unwrap();
                    cache
                },
                |cache| {
                    for t in 0..256 {
                        cache.write_kv(t, &keys, &values).unwrap();
                        black_box(cache.read_range(0, t + 1).unwrap());
                    }
                },
                BatchSize::SmallInput,
            );
        });

        // 6b. decode_step_256_zero_copy
        group.bench_function(
            BenchmarkId::new("decode_step_256_zero_copy", config_name),
            |b| {
                b.iter_batched(
                    || {
                        let cache = LayerKvCache::new(config);
                        cache.initialize(256).unwrap();
                        cache
                    },
                    |cache| {
                        for t in 0..256 {
                            cache.write_kv(t, &keys, &values).unwrap();
                            cache
                                .with_read_range(0, t + 1, |s| {
                                    black_box(s.keys.len());
                                })
                                .unwrap();
                        }
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        // 7. concurrent: 4 readers looping read_range(0, 256) on a prefilled cache
        group.bench_function(
            BenchmarkId::new("concurrent_4_readers_256", config_name),
            |b| {
                let cache_arc = prefilled.clone();
                b.iter_custom(|iters| {
                    let start = Instant::now();
                    let handles: Vec<_> = (0..4)
                        .map(|_| {
                            let c = cache_arc.clone();
                            std::thread::spawn(move || {
                                for _ in 0..iters {
                                    black_box(c.read_range(0, 256).unwrap());
                                }
                            })
                        })
                        .collect();
                    for h in handles {
                        h.join().unwrap();
                    }
                    start.elapsed()
                });
            },
        );

        // 7b. concurrent_4_readers_256_zero_copy
        group.bench_function(
            BenchmarkId::new("concurrent_4_readers_256_zero_copy", config_name),
            |b| {
                let cache_arc = prefilled.clone();
                b.iter_custom(|iters| {
                    let start = Instant::now();
                    let handles: Vec<_> = (0..4)
                        .map(|_| {
                            let c = cache_arc.clone();
                            std::thread::spawn(move || {
                                for _ in 0..iters {
                                    c.with_read_range(0, 256, |s| {
                                        black_box(s.keys.len());
                                    })
                                    .unwrap();
                                }
                            })
                        })
                        .collect();
                    for h in handles {
                        h.join().unwrap();
                    }
                    start.elapsed()
                });
            },
        );

        group.finish();
    }
}

criterion_group!(benches, bench_kv_cache);
criterion_main!(benches);
