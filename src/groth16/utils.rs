use std::sync::atomic::{AtomicUsize, Ordering};

use ff::PrimeField;
use groupy::{CurveAffine, CurveProjective};
use lazy_static::lazy_static;
use rayon::prelude::*;

use crate::bls::Engine;

pub const WINDOW_SIZE: usize = 8;

lazy_static! {
    pub static ref POOL: rayon::ThreadPool = {
        let num_threads = num_cpus::get().max(6);

        rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build()
            .unwrap()
    };
}

#[cfg(target_arch = "x86_64")]
fn prefetch<T>(p: *const T) {
    unsafe {
        core::arch::x86_64::_mm_prefetch(p as *const _, core::arch::x86_64::_MM_HINT_T0);
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn prefetch<T>(p: *const T) {}

pub enum PublicInputs<'a, E: Engine, F: Fn(usize) -> <E::Fr as PrimeField>::Repr + Sync + Send> {
    Slice(&'a [<E::Fr as PrimeField>::Repr]),
    Getter(F),
}

pub type Getter<E> =
    dyn Fn(usize) -> <<E as ff::ScalarEngine>::Fr as PrimeField>::Repr + Sync + Send;

impl<'a, E: Engine, F: Fn(usize) -> <E::Fr as PrimeField>::Repr + Sync + Send>
    PublicInputs<'a, E, F>
{
    pub fn get(&self, i: usize) -> <E::Fr as PrimeField>::Repr {
        match self {
            PublicInputs::Slice(inputs) => inputs[i],
            PublicInputs::Getter(f) => f(i),
        }
    }
}

pub trait MultiscalarPrecomp<E: Engine>: Send + Sync {
    fn window_size(&self) -> usize;
    fn window_mask(&self) -> u64;
    fn tables(&self) -> &[Vec<E::G1Affine>];
    fn at_point(&self, idx: usize) -> MultiscalarPrecompRef<'_, E>;
}

#[derive(Debug)]
pub struct MultiscalarPrecompOwned<E: Engine> {
    num_points: usize,
    window_size: usize,
    window_mask: u64,
    table_entries: usize,
    tables: Vec<Vec<E::G1Affine>>,
}

impl<E: Engine> MultiscalarPrecomp<E> for MultiscalarPrecompOwned<E> {
    fn window_size(&self) -> usize {
        self.window_size
    }

    fn window_mask(&self) -> u64 {
        self.window_mask
    }

    fn tables(&self) -> &[Vec<E::G1Affine>] {
        &self.tables
    }

    fn at_point(&self, idx: usize) -> MultiscalarPrecompRef<'_, E> {
        MultiscalarPrecompRef {
            num_points: self.num_points - idx,
            window_size: self.window_size,
            window_mask: self.window_mask,
            table_entries: self.table_entries,
            tables: &self.tables[idx..],
        }
    }
}

pub struct MultiscalarPrecompRef<'a, E: Engine> {
    num_points: usize,
    window_size: usize,
    window_mask: u64,
    table_entries: usize,
    tables: &'a [Vec<E::G1Affine>],
}

impl<E: Engine> MultiscalarPrecomp<E> for MultiscalarPrecompRef<'_, E> {
    fn window_size(&self) -> usize {
        self.window_size
    }

    fn window_mask(&self) -> u64 {
        self.window_mask
    }

    fn tables(&self) -> &[Vec<E::G1Affine>] {
        self.tables
    }

    fn at_point(&self, idx: usize) -> MultiscalarPrecompRef<'_, E> {
        MultiscalarPrecompRef {
            num_points: self.num_points - idx,
            window_size: self.window_size,
            window_mask: self.window_mask,
            table_entries: self.table_entries,
            tables: &self.tables[idx..],
        }
    }
}

/// Precompute tables for fixed bases.
pub fn precompute_fixed_window<E: Engine>(
    points: &[E::G1Affine],
    window_size: usize,
) -> MultiscalarPrecompOwned<E> {
    let table_entries = (1 << window_size) - 1;
    let num_points = points.len();

    let tables = points
        .into_par_iter()
        .map(|point| {
            let mut table = Vec::with_capacity(table_entries);
            table.push(*point);

            let mut cur_precomp_point = point.into_projective();

            for _ in 1..table_entries {
                cur_precomp_point.add_assign_mixed(point);
                table.push(cur_precomp_point.into_affine());
            }

            table
        })
        .collect();

    MultiscalarPrecompOwned {
        num_points,
        window_size,
        window_mask: (1 << window_size) - 1,
        table_entries,
        tables,
    }
}

/// Multipoint scalar multiplication
/// Only supports window sizes that evenly divide a limb and nbits!!
fn multiscalar<E: Engine>(
    k: &[<E::Fr as ff::PrimeField>::Repr],
    precomp_table: &dyn MultiscalarPrecomp<E>,
    num_points: usize,
    nbits: usize,
) -> E::G1 {
    // TODO: support more bit sizes
    if nbits % precomp_table.window_size() != 0
        || std::mem::size_of::<u64>() * 8 % precomp_table.window_size() != 0
    {
        panic!("Unsupported multiscalar window size!");
    }

    let mut result = E::G1::zero();

    // nbits must be evenly divided by window_size!
    let num_windows = (nbits + precomp_table.window_size() - 1) / precomp_table.window_size();
    let mut idx;

    // This version prefetches the next window and computes on the previous window.
    for i in (0..num_windows).rev() {
        const BITS_PER_LIMB: usize = std::mem::size_of::<u64>() * 8;
        let limb = (i * precomp_table.window_size()) / BITS_PER_LIMB;
        let window_in_limb = i % (BITS_PER_LIMB / precomp_table.window_size());

        for _ in 0..precomp_table.window_size() {
            result.double();
        }
        let mut prev_idx = 0;
        let mut prev_table: &Vec<E::G1Affine> = &precomp_table.tables()[0];
        let mut table: &Vec<E::G1Affine> = &precomp_table.tables()[0];
        for m in 0..num_points {
            idx = (AsRef::<[u64]>::as_ref(&k[m]))[limb]
                >> (window_in_limb * precomp_table.window_size())
                & precomp_table.window_mask();
            if idx > 0 {
                table = &precomp_table.tables()[m];
                prefetch(&table[idx as usize - 1]);
            }
            if prev_idx > 0 && m > 0 {
                result.add_assign_mixed(&prev_table[prev_idx as usize - 1]);
            }
            prev_idx = idx;
            prev_table = table;
        }
        // Perform the final addition
        if prev_idx > 0 {
            result.add_assign_mixed(&prev_table[prev_idx as usize - 1]);
        }
    }

    result
}

/// Perform a threaded multiscalar multiplication and accumulation.
pub fn par_multiscalar<F, E: Engine>(
    max_threads: usize,
    k: &PublicInputs<'_, E, F>,
    precomp_table: &dyn MultiscalarPrecomp<E>,
    num_points: usize,
    nbits: usize,
) -> E::G1
where
    F: Fn(usize) -> <E::Fr as PrimeField>::Repr + Sync + Send,
{
    // The granularity of work, in points. When a thread gets work it will
    // gather chunk_size points, perform muliscalar on them, and accumulate
    // the result. This is more efficient than evenly dividing the work among
    // threads because threads sometimes get preempted. When that happens
    // these long pole threads hold up progress across the board resulting in
    // occasional long delays.
    let mut chunk_size = 16; // TUNEABLE
    if num_points > 1024 {
        chunk_size = 256;
    }
    if chunk_size > num_points {
        chunk_size = 1; // fallback for tests and tiny inputs
    }

    let num_threads = max_threads.min((num_points + chunk_size - 1) / chunk_size);

    // Work item counter - each thread will take work by incrementing
    let work = AtomicUsize::new(0);

    let acc_intermediates = POOL.install(|| {
        (0..num_threads)
            .into_par_iter()
            .map(|_tid| {
                // Temporary storage for scalars
                let mut scalar_storage = vec![<E::Fr as PrimeField>::Repr::default(); chunk_size];

                // Thread result accumulation
                let mut thr_result = E::G1::zero();

                loop {
                    let i = work.fetch_add(1, Ordering::SeqCst);
                    let start_idx = i * chunk_size;
                    if start_idx >= num_points {
                        break;
                    }

                    let mut end_idx = start_idx + chunk_size;
                    if end_idx > num_points {
                        end_idx = num_points;
                    }
                    let num_items = end_idx - start_idx;

                    let scalars = match k {
                        PublicInputs::Slice(ref s) => &s[start_idx..],
                        PublicInputs::Getter(ref getter) => {
                            for i in start_idx..end_idx {
                                scalar_storage[i - start_idx] = getter(i);
                            }
                            &scalar_storage
                        }
                    };
                    let subset = precomp_table.at_point(start_idx);
                    let acc = multiscalar(scalars, &subset, num_items, nbits);
                    drop(scalars);
                    thr_result.add_assign(&acc);
                }
                thr_result
            })
            .collect::<Vec<_>>()
    });

    let mut result = E::G1::zero();

    // Accumulate thread results
    for acc in acc_intermediates {
        result.add_assign(&acc);
    }

    result
}