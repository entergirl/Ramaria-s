//! Phase 0 POC: 验证本地向量存储 + 检索 + 持久化
//!
//! 纯 Rust 实现，无外部向量库依赖。
//! `cargo run --example vector_poc -p ramaria-cli`

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const DIM: usize = 128;
const PATH: &str = "target/poc_vectors.json";

#[derive(Debug, Serialize, Deserialize)]
struct VectorStore {
    vectors: HashMap<u64, Vec<f32>>,
}

fn main() {
    let seed = 42u64;
    let query = random_vec(seed);

    // ── 1. 构建向量库 ──────────────────────────────────
    let mut store = VectorStore {
        vectors: HashMap::new(),
    };
    for id in 1u64..=10 {
        let v = if id == 5 {
            noisy_copy(&query, 0.01) // id=5 与查询最接近
        } else {
            random_vec(seed + id)
        };
        store.vectors.insert(id, v);
    }

    // ── 2. 余弦相似度检索 Top-3 ────────────────────────
    let mut scored: Vec<(u64, f32)> = store
        .vectors
        .iter()
        .map(|(&id, v)| (id, cosine_sim(&query, v)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    println!("query → top-3:");
    for (id, score) in scored.iter().take(3) {
        println!("  id={id}  sim={:.4}", score);
    }
    assert_eq!(scored[0].0, 5, "expected id=5 nearest, got {}", scored[0].0);
    println!("  ✓ id=5 is nearest");

    // ── 3. 持久化到 JSON ───────────────────────────────
    let json = serde_json::to_string(&store).expect("serialize failed");
    std::fs::write(PATH, &json).expect("write failed");
    let meta = std::fs::metadata(PATH).expect("file not found");
    println!("\nsaved: {PATH}  ({:.1} KB)", meta.len() as f64 / 1024.0);

    // 释放旧数据
    drop(store);

    // ── 4. 从 JSON 加载，验证数据完整 ──────────────────
    let raw = std::fs::read_to_string(PATH).expect("read failed");
    let store2: VectorStore = serde_json::from_str(&raw).expect("deserialize failed");

    let best = store2
        .vectors
        .iter()
        .map(|(&id, v)| (id, cosine_sim(&query, v)))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .unwrap();
    assert_eq!(best.0, 5, "persisted search wrong");
    println!("reload: top-1 = id={}, sim={:.4}", best.0, best.1);
    println!("  ✓ persistence ok");

    // ── 5. 清理 ────────────────────────────────────────
    std::fs::remove_file(PATH).expect("cleanup failed");

    println!("\nPASS -- vector search ok");
}

fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    let (dot, na, nb) = a
        .iter()
        .zip(b.iter())
        .fold((0.0f32, 0.0f32, 0.0f32), |(d, na, nb), (&x, &y)| {
            (d + x * y, na + x * x, nb + y * y)
        });
    dot / (na.sqrt() * nb.sqrt())
}

fn random_vec(seed: u64) -> Vec<f32> {
    let mut s = seed;
    (0..DIM)
        .map(|_| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((s >> 33) as u32) as f32 / u32::MAX as f32 * 2.0 - 1.0
        })
        .collect()
}

fn noisy_copy(src: &[f32], noise: f32) -> Vec<f32> {
    let mut s = 99u64;
    src.iter()
        .map(|&x| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
            let n = ((s >> 33) as u32) as f32 / u32::MAX as f32 * noise * 2.0 - noise;
            x + n
        })
        .collect()
}
