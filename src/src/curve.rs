//! Fan curve parsing, validation and interpolation.
//!
//! The interpolation reproduces the shell controller's integer arithmetic
//! (truncating division) exactly, so a given curve and temperature always
//! produce the same PWM value as before the migration.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurvePoint {
    pub temp: i32,
    pub percent: i32,
}

fn is_uint(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Parses `temp:percent,temp:percent,...`. Returns `None` when any point is
/// malformed, out of range (temp > 150, percent > 100) or temperatures are not
/// strictly increasing.
pub fn parse_curve(text: &str) -> Option<Vec<CurvePoint>> {
    let mut points: Vec<CurvePoint> = Vec::new();
    for pair in text.split(',') {
        let (t, p) = pair.split_once(':')?;
        if !is_uint(t) || !is_uint(p) {
            return None;
        }
        let temp: i32 = t.parse().ok()?;
        let percent: i32 = p.parse().ok()?;
        if temp > 150 || percent > 100 {
            return None;
        }
        if let Some(last) = points.last() {
            if temp <= last.temp {
                return None;
            }
        }
        points.push(CurvePoint { temp, percent });
    }
    Some(points)
}

/// PWM value (0..255) the controller requests at `temp`.
///
/// Below or at the first point the first percent applies, above the last
/// point the last percent applies, and in between the segment is linearly
/// interpolated using the shell controller's integer arithmetic.
pub fn curve_pwm(temp: i32, points: &[CurvePoint]) -> Option<u32> {
    let mut prev: Option<CurvePoint> = None;
    for point in points {
        if temp <= point.temp {
            let result = match prev {
                None => point.percent,
                Some(prev) => {
                    prev.percent
                        + (temp - prev.temp) * (point.percent - prev.percent)
                            / (point.temp - prev.temp)
                }
            };
            return Some((result * 255 / 100).clamp(0, 255) as u32);
        }
        prev = Some(*point);
    }
    prev.map(|last| (last.percent * 255 / 100).clamp(0, 255) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(text: &str) -> Vec<CurvePoint> {
        parse_curve(text).unwrap()
    }

    #[test]
    fn parses_valid_curves() {
        let points = pts("20:0,40:50,70:100");
        assert_eq!(points.len(), 3);
        assert_eq!(
            points[1],
            CurvePoint {
                temp: 40,
                percent: 50
            }
        );
    }

    #[test]
    fn rejects_malformed_curves() {
        for bad in [
            "",
            "20",
            "20:",
            ":50",
            "20:50,",
            "a:50",
            "20:-5",
            "20:101",
            "151:50",
            "40:50,40:60",
            "40:50,30:60",
            "40:50, 45:60",
        ] {
            assert!(parse_curve(bad).is_none(), "should reject {bad:?}");
        }
    }

    #[test]
    fn boundary_and_flat_segments_match_shell_math() {
        let points = pts("20:0,35:0,40:50,55:50,70:60,85:75,95:100");
        // Below first point -> first percent.
        assert_eq!(curve_pwm(10, &points), Some(0));
        // Exactly on a point -> that point's percent.
        assert_eq!(curve_pwm(35, &points), Some(0));
        assert_eq!(curve_pwm(40, &points), Some(127)); // 50*255/100 = 127
                                                       // Between 40 and 55 the percent stays 50 -> 127.
        assert_eq!(curve_pwm(48, &points), Some(127));
        // Between 70 and 85 at 77: 60 + 7*15/15 = 67 -> 67*255/100 = 170 (trunc).
        assert_eq!(curve_pwm(77, &points), Some(170));
        // Above the last point -> last percent.
        assert_eq!(curve_pwm(120, &points), Some(255));
    }

    #[test]
    fn interpolation_truncates_like_shell() {
        let points = pts("40:50,55:60");
        // On the 55:60 point -> 60% -> 153.
        assert_eq!(curve_pwm(55, &points), Some(153));
        // 43°C: 50 + 3*10/15 = 50 + 2 (integer division) = 52 -> 132.
        assert_eq!(curve_pwm(43, &points), Some(132));
        // 42°C: 50 + 2*10/15 = 50 + 1 = 51 -> 130.
        assert_eq!(curve_pwm(42, &points), Some(130));
    }

    #[test]
    fn empty_curve_has_no_output() {
        assert_eq!(curve_pwm(50, &[]), None);
    }
}
