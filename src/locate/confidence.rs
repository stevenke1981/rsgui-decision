//! Normalized per-channel correlation. Integral images supply patch statistics;
//! FFT supplies the cross term for large searches, with direct verification at
//! the acceptance threshold. No native OpenCV runtime is required.
use super::{luma, suppress_overlap};
use crate::error::Result;
use crate::types::{Rect, Screenshot};
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::sync::Arc;

type Check<'a> = &'a mut dyn FnMut() -> Result<()>;

struct Template {
    channels: usize,
    means: Vec<f64>,
    centered: Vec<Vec<f64>>,
    variance: f64,
}

fn value(image: &Screenshot, index: usize, channel: usize, gray: bool) -> f64 {
    let p = &image.pixels[index * 3..index * 3 + 3];
    if gray {
        f64::from(luma(p))
    } else {
        f64::from(p[channel])
    }
}

impl Template {
    fn new(needle: &Screenshot, gray: bool) -> Self {
        let channels = if gray { 1 } else { 3 };
        let count = needle.width as usize * needle.height as usize;
        let mut means = Vec::with_capacity(channels);
        let mut centered = Vec::with_capacity(channels);
        let mut variance = 0.0;
        let pixels = needle.pixels.as_chunks::<3>().0;
        for channel in 0..channels {
            let mut sum = 0u64;
            let mut plane = Vec::with_capacity(count);
            for p in pixels {
                let v = if gray { luma(p) } else { p[channel] };
                sum += u64::from(v);
                plane.push(f64::from(v));
            }
            let mean = sum as f64 / count as f64;
            for v in &mut plane {
                *v -= mean;
                variance += *v * *v;
            }
            means.push(mean);
            centered.push(plane);
        }
        Self {
            channels,
            means,
            centered,
            variance,
        }
    }

    fn score_at(&self, hay: &Screenshot, needle: &Screenshot, x: usize, y: usize) -> f64 {
        let count = u64::from(needle.width) * u64::from(needle.height);
        let mut covariance = 0.0;
        let mut patch_variance = 0.0;
        let mut squared_error = 0.0;
        for channel in 0..self.channels {
            let mut sum = 0u64;
            let mut square_sum = 0u64;
            for ny in 0..needle.height as usize {
                for nx in 0..needle.width as usize {
                    let i = ny * needle.width as usize + nx;
                    let h = value(
                        hay,
                        (y + ny) * hay.width as usize + x + nx,
                        channel,
                        self.channels == 1,
                    );
                    let n = self.centered[channel][i];
                    covariance += (h - self.means[channel]) * n;
                    sum += h as u64;
                    square_sum += (h * h) as u64;
                    squared_error += (h - n - self.means[channel]).powi(2);
                }
            }
            patch_variance += (u128::from(square_sum) * u128::from(count) - u128::from(sum).pow(2))
                as f64
                / count as f64;
        }
        if self.variance <= 1e-9 {
            // A constant template has no normalized correlation. Compare color
            // distance instead of accepting every constant patch as score 1.
            1.0 - (squared_error / (count as f64 * self.channels as f64)).sqrt() / 255.0
        } else if patch_variance <= 1e-9 {
            0.0
        } else {
            (covariance / (self.variance * patch_variance).sqrt()).clamp(-1.0, 1.0)
        }
    }
}

pub(super) fn search(
    hay: &Screenshot,
    needle: &Screenshot,
    gray: bool,
    threshold: f32,
    check: Check<'_>,
) -> Result<Vec<Rect>> {
    let template = Template::new(needle, gray);
    let columns = (hay.width - needle.width + 1) as usize;
    let rows = (hay.height - needle.height + 1) as usize;
    let workload = (columns as u64)
        .saturating_mul(rows as u64)
        .saturating_mul(u64::from(needle.width))
        .saturating_mul(u64::from(needle.height))
        .saturating_mul(template.channels as u64);
    let mut candidates = Vec::new();
    if workload <= 2_000_000 {
        for y in 0..rows {
            check()?;
            for x in 0..columns {
                let score = template.score_at(hay, needle, x, y);
                if score + 1e-12 >= f64::from(threshold) {
                    candidates.push((
                        Rect::new(
                            x as i32,
                            y as i32,
                            needle.width as i32,
                            needle.height as i32,
                        ),
                        score,
                    ));
                }
            }
        }
    } else {
        let stats = patch_statistics(hay, needle, &template, check)?;
        let cross = if template.variance > 1e-9 {
            Some(cross_correlation(hay, needle, &template, check)?)
        } else {
            None
        };
        for y in 0..rows {
            check()?;
            for x in 0..columns {
                let index = y * columns + x;
                let approximate = match &cross {
                    Some(cross) if stats[index] > 1e-9 => {
                        cross[index] / (template.variance * stats[index]).sqrt()
                    }
                    Some(_) => 0.0,
                    None => {
                        1.0 - (stats[index]
                            / (f64::from(needle.width)
                                * f64::from(needle.height)
                                * template.channels as f64))
                            .sqrt()
                            / 255.0
                    }
                };
                // FFT is a candidate filter. Verify every accepted correlation
                // directly so numerical error cannot authorize an input action.
                if approximate + 1e-8 >= f64::from(threshold) {
                    if x % 32 == 0 {
                        check()?;
                    }
                    let score = if cross.is_some() {
                        template.score_at(hay, needle, x, y)
                    } else {
                        approximate.clamp(-1.0, 1.0)
                    };
                    if score + 1e-12 >= f64::from(threshold) {
                        candidates.push((
                            Rect::new(
                                x as i32,
                                y as i32,
                                needle.width as i32,
                                needle.height as i32,
                            ),
                            score,
                        ));
                    }
                }
            }
        }
    }
    check()?;
    candidates.sort_unstable_by(|(a, sa), (b, sb)| {
        sb.total_cmp(sa)
            .then_with(|| a.top.cmp(&b.top))
            .then_with(|| a.left.cmp(&b.left))
    });
    suppress_overlap(
        candidates.into_iter().map(|(rect, _)| rect).collect(),
        check,
    )
}

// One channel's integral tables at a time, so auxiliary memory is independent
// of channel count. Output is patch variance or, for flat templates, squared error.
fn patch_statistics(
    hay: &Screenshot,
    needle: &Screenshot,
    template: &Template,
    check: Check<'_>,
) -> Result<Vec<f64>> {
    let stride = hay.width as usize + 1;
    let rows = hay.height as usize + 1;
    let columns = (hay.width - needle.width + 1) as usize;
    let output_rows = (hay.height - needle.height + 1) as usize;
    let mut output = vec![0.0; columns * output_rows];
    let mut sums = vec![0u64; stride * rows];
    let mut squares = vec![0u64; stride * rows];
    let count = u128::from(needle.width) * u128::from(needle.height);
    let hay_stride = hay.width as usize * 3;
    for channel in 0..template.channels {
        let gray = template.channels == 1;
        for y in 1..rows {
            if y % 32 == 0 {
                check()?;
            }
            let mut sum = 0u64;
            let mut square = 0u64;
            let row_start = (y - 1) * hay_stride;
            let row = &hay.pixels[row_start..row_start + hay_stride];
            for (x, p) in (1..stride).zip(row.as_chunks::<3>().0.iter()) {
                let v = u64::from(if gray { luma(p) } else { p[channel] });
                sum += v;
                square += v * v;
                sums[y * stride + x] = sums[(y - 1) * stride + x] + sum;
                squares[y * stride + x] = squares[(y - 1) * stride + x] + square;
            }
        }
        for y in 0..output_rows {
            if y % 32 == 0 {
                check()?;
            }
            for x in 0..columns {
                let a = y * stride + x;
                let b = a + needle.width as usize;
                let c = (y + needle.height as usize) * stride + x;
                let d = c + needle.width as usize;
                let sum = u128::from((sums[d] - sums[b]) - (sums[c] - sums[a]));
                let square = u128::from((squares[d] - squares[b]) - (squares[c] - squares[a]));
                let statistic = if template.variance <= 1e-9 {
                    let mean = template.means[channel] as u128;
                    (square + count * mean * mean - 2 * mean * sum) as f64
                } else {
                    // Keep the subtraction exact for nearly flat, bright UI.
                    (square * count - sum * sum) as f64 / count as f64
                };
                output[y * columns + x] += statistic;
            }
        }
    }
    Ok(output)
}

// RustFFT is fastest with dimensions factored into 2 and 3. Valid-correlation
// positions do not wrap, so padding to the image extent is sufficient.
fn fft_len(minimum: usize) -> usize {
    let mut best = minimum.next_power_of_two();
    let mut two = 1usize;
    while two <= best {
        let mut length = two;
        while length < minimum {
            let Some(next) = length.checked_mul(3) else {
                break;
            };
            length = next;
        }
        if length >= minimum {
            best = best.min(length);
        }
        let Some(next) = two.checked_mul(2) else {
            break;
        };
        two = next;
    }
    best
}

struct Transform {
    width: usize,
    height: usize,
    row: Arc<dyn Fft<f64>>,
    column: Arc<dyn Fft<f64>>,
}

impl Transform {
    fn run(
        &self,
        buffer: &mut [Complex<f64>],
        scratch: &mut [Complex<f64>],
        column: &mut [Complex<f64>],
        check: Check<'_>,
    ) -> Result<()> {
        for (index, row) in buffer.chunks_exact_mut(self.width).enumerate() {
            if index % 32 == 0 {
                check()?;
            }
            self.row.process_with_scratch(row, scratch);
        }
        for x in 0..self.width {
            if x % 32 == 0 {
                check()?;
            }
            for y in 0..self.height {
                column[y] = buffer[y * self.width + x];
            }
            self.column.process_with_scratch(column, scratch);
            for y in 0..self.height {
                buffer[y * self.width + x] = column[y];
            }
        }
        Ok(())
    }
}

fn cross_correlation(
    hay: &Screenshot,
    needle: &Screenshot,
    template: &Template,
    check: Check<'_>,
) -> Result<Vec<f64>> {
    let columns = (hay.width - needle.width + 1) as usize;
    let rows = (hay.height - needle.height + 1) as usize;
    let active_channels: Vec<usize> = (0..template.channels)
        .filter(|&channel| template.centered[channel].iter().any(|v| v.abs() > 1e-12))
        .collect();
    if active_channels.is_empty() {
        return Ok(vec![0.0; columns * rows]);
    }
    let width = fft_len(hay.width as usize);
    let height = fft_len(hay.height as usize);
    let length = width * height;
    let mut planner = FftPlanner::<f64>::new();
    let forward = Transform {
        width,
        height,
        row: planner.plan_fft_forward(width),
        column: planner.plan_fft_forward(height),
    };
    let inverse = Transform {
        width,
        height,
        row: planner.plan_fft_inverse(width),
        column: planner.plan_fft_inverse(height),
    };
    let scratch_len = forward
        .row
        .get_inplace_scratch_len()
        .max(forward.column.get_inplace_scratch_len())
        .max(inverse.row.get_inplace_scratch_len())
        .max(inverse.column.get_inplace_scratch_len());
    let mut scratch = vec![Complex::default(); scratch_len];
    let mut column = vec![Complex::default(); height];
    let mut image = vec![Complex::default(); length];
    let mut filter = vec![Complex::default(); length];
    let mut result = if active_channels.len() > 1 {
        Some(vec![Complex::default(); length])
    } else {
        None
    };
    let hay_stride = hay.width as usize * 3;
    let gray = template.channels == 1;
    for (pass, &channel) in active_channels.iter().enumerate() {
        check()?;
        if pass > 0 {
            image.fill(Complex::default());
            filter.fill(Complex::default());
        }
        for y in 0..hay.height as usize {
            let row_start = y * hay_stride;
            let row = &hay.pixels[row_start..row_start + hay_stride];
            let dst = &mut image[y * width..y * width + hay.width as usize];
            for (slot, p) in dst.iter_mut().zip(row.as_chunks::<3>().0.iter()) {
                slot.re = f64::from(if gray { luma(p) } else { p[channel] });
            }
        }
        for y in 0..needle.height as usize {
            let src_start = y * needle.width as usize;
            let src = &template.centered[channel][src_start..src_start + needle.width as usize];
            let dst = &mut filter[y * width..y * width + needle.width as usize];
            for (slot, &val) in dst.iter_mut().zip(src) {
                slot.re = val;
            }
        }
        forward.run(&mut image, &mut scratch, &mut column, check)?;
        forward.run(&mut filter, &mut scratch, &mut column, check)?;
        if let Some(acc) = result.as_mut() {
            for i in 0..length {
                acc[i] += image[i] * filter[i].conj();
            }
        } else {
            for i in 0..length {
                image[i] *= filter[i].conj();
            }
        }
    }
    drop(filter);
    let output_buf = if let Some(mut acc) = result {
        drop(image);
        inverse.run(&mut acc, &mut scratch, &mut column, check)?;
        acc
    } else {
        inverse.run(&mut image, &mut scratch, &mut column, check)?;
        image
    };
    let mut output = Vec::with_capacity(columns * rows);
    for y in 0..rows {
        for x in 0..columns {
            output.push(output_buf[y * width + x].re / length as f64);
        }
    }
    Ok(output)
}
