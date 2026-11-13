//! Color dominante de una portada y su versión de fondo (portado de `CoverColors.swift`).

pub type Rgb = [u8; 3];

/// RGB (0..=255) → (h 0..1, s 0..1, v 0..1).
pub fn to_hsv([r, g, b]: Rgb) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    let s = if max == 0.0 { 0.0 } else { d / max };
    (h, s, max)
}

/// (h, s, v) → RGB.
pub fn from_hsv(h: f32, s: f32, v: f32) -> Rgb {
    let h = h.rem_euclid(1.0) * 6.0;
    let (c, i) = (v * s, h.floor() as i32);
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = match i {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let q = |n: f32| ((n + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [q(r), q(g), q(b)]
}

/// Color dominante vivo de una imagen reducida (RGBA, p. ej. 24×24). `None` si no hay píxeles útiles.
pub fn dominant(rgba: &[u8]) -> Option<Rgb> {
    let total = rgba.len() / 4;
    if total == 0 {
        return None;
    }
    let (mut r, mut g, mut b, mut wsum) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let (mut vivid, mut vsum) = (0usize, 0.0f64);
    for px in rgba.chunks_exact(4) {
        let (_, s, v) = to_hsv([px[0], px[1], px[2]]);
        vsum += v as f64;
        if s > 0.25 && v > 0.2 {
            vivid += 1;
        }
        let w = (s * s * v) as f64 + 0.02; // favorece colores vivos
        r += px[0] as f64 * w;
        g += px[1] as f64 * w;
        b += px[2] as f64 * w;
        wsum += w;
    }
    // Portada casi gris/negra/blanca: se queda en neutro, sin inventar un color.
    if (vivid as f64) / (total as f64) < 0.05 {
        let level = ((vsum / total as f64).clamp(0.62, 0.85) * 255.0).round() as u8;
        return Some([level; 3]);
    }
    let avg = [(r / wsum).round() as u8, (g / wsum).round() as u8, (b / wsum).round() as u8];
    let (h, s, v) = to_hsv(avg);
    Some(from_hsv(h, s.clamp(0.45, 0.9), v.clamp(0.6, 0.9)))
}

/// Versión oscura y profunda del color, para fondo de ventana. Los neutros siguen siendo neutros.
pub fn deep(c: Rgb) -> Rgb {
    let (h, s, _) = to_hsv(c);
    if s < 0.12 {
        return [51, 51, 51];
    }
    from_hsv(h, s.clamp(0.5, 0.85), 0.36)
}

/// Tono medio vivo de la portada para botones y fichas con texto blanco legible.
pub fn pill(c: Rgb) -> Rgb {
    let (h, s, _) = to_hsv(c);
    if s < 0.12 {
        return [78, 78, 78];
    }
    from_hsv(h, s.clamp(0.55, 0.85), 0.56)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(rgb: Rgb, n: usize) -> Vec<u8> {
        (0..n).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect()
    }

    #[test]
    fn hsv_roundtrip() {
        for c in [[200, 30, 60], [10, 200, 90], [30, 60, 220], [128, 128, 128]] {
            let (h, s, v) = to_hsv(c);
            let back = from_hsv(h, s, v);
            for i in 0..3 {
                assert!((back[i] as i32 - c[i] as i32).abs() <= 1, "{c:?} → {back:?}");
            }
        }
    }

    #[test]
    fn vivid_cover_keeps_its_hue() {
        let c = dominant(&solid([220, 20, 30], 576)).unwrap();
        let (h, s, v) = to_hsv(c);
        assert!(h < 0.03 || h > 0.97, "tono rojo, no {h}");
        // Tolerancia de 0.01: pasar por enteros de 8 bits pierde algo de precisión.
        assert!((0.45..=0.91).contains(&s) && (0.6..=0.91).contains(&v), "s={s} v={v}");
    }

    #[test]
    fn gray_cover_stays_neutral() {
        let c = dominant(&solid([20, 20, 20], 576)).unwrap();
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert!(c[0] >= 158); // mínimo 0.62
    }

    #[test]
    fn deep_is_dark_and_neutral_stays_gray() {
        let d = deep([220, 20, 30]);
        assert!(to_hsv(d).2 <= 0.37);
        assert_eq!(deep([128, 128, 128]), [51, 51, 51]);
    }

    #[test]
    fn empty_image_has_no_color() {
        assert_eq!(dominant(&[]), None);
    }

    #[test]
    fn pill_keeps_readable_contrast_and_neutral_gray() {
        assert_eq!(pill([128, 128, 128]), [78, 78, 78]);
        let p = pill([30, 120, 220]);
        let (_, s, v) = to_hsv(p);
        assert!((0.55..=0.86).contains(&s) && (0.54..=0.58).contains(&v));
    }
}
