//! Fan curve parsing and output computation.
//!
//! The arithmetic is byte-for-byte equivalent to the 2.x shell implementation
//! (`prev_p + (temp - prev_t) * (p - prev_p) / (t - prev_t)` followed by
//! `percent * 255 / 100`, both truncating towards zero) so that an existing
//! device keeps the exact same fan behaviour after the migration.

use std::fmt;

pub const MAX_TEMP_C: i32 = 150;
pub const MAX_PERCENT: i32 = 100;
pub const PWM_SPAN: i32 = 255;
pub const MAX_POINTS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurvePoint {
    pub temp_c: i32,
    pub percent: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurveError {
    /// No usable point at all.
    Empty,
    /// A token is not `temperature:percent` with non-negative integers.
    Malformed { token: String, reason: String },
    /// Duplicate or decreasing temperature; the curve would be ambiguous.
    NotIncreasing { temp_c: i32, previous: i32 },
    /// Temperature above the physically plausible sensor range.
    TempOutOfRange { temp_c: i32 },
    /// Percentage outside 0..100.
    PercentOutOfRange { percent: i32 },
    /// More points than supported.
    TooManyPoints { count: usize },
}

impl fmt::Display for CurveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CurveError::Empty => write!(f, "curve has no valid point"),
            CurveError::Malformed { token, reason } => {
                write!(f, "curve point {token:?} is not usable ({reason})")
            }
            CurveError::NotIncreasing { temp_c, previous } => write!(
                f,
                "curve temperature {temp_c} does not increase from {previous}"
            ),
            CurveError::TempOutOfRange { temp_c } => {
                write!(f, "curve temperature {temp_c} is above {MAX_TEMP_C}")
            }
            CurveError::PercentOutOfRange { percent } => {
                write!(f, "curve percentage {percent} is outside 0..{MAX_PERCENT}")
            }
            CurveError::TooManyPoints { count } => {
                write!(
                    f,
                    "curve has {count} points, at most {MAX_POINTS} are supported"
                )
            }
        }
    }
}

fn parse_uint(token: &str) -> Option<i32> {
    let text = token.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<i32>().ok()
}

/// Parse `20:0,40:50,...` into strictly increasing, in-range points.
pub fn parse(spec: &str) -> Result<Vec<CurvePoint>, CurveError> {
    let mut points: Vec<CurvePoint> = Vec::new();
    for token in spec.split(',') {
        if token.trim().is_empty() {
            // A trailing comma is harmless and the shell loop skipped it too.
            continue;
        }
        let Some((raw_temp, raw_percent)) = token.split_once(':') else {
            return Err(CurveError::Malformed {
                token: token.trim().to_string(),
                reason: "expected temperature:percent".to_string(),
            });
        };
        let Some(temp_c) = parse_uint(raw_temp) else {
            return Err(CurveError::Malformed {
                token: token.trim().to_string(),
                reason: "temperature must be a non-negative integer".to_string(),
            });
        };
        let Some(percent) = parse_uint(raw_percent) else {
            return Err(CurveError::Malformed {
                token: token.trim().to_string(),
                reason: "percent must be a non-negative integer".to_string(),
            });
        };
        if temp_c > MAX_TEMP_C {
            return Err(CurveError::TempOutOfRange { temp_c });
        }
        if percent > MAX_PERCENT {
            return Err(CurveError::PercentOutOfRange { percent });
        }
        if let Some(last) = points.last() {
            if temp_c <= last.temp_c {
                return Err(CurveError::NotIncreasing {
                    temp_c,
                    previous: last.temp_c,
                });
            }
        }
        points.push(CurvePoint { temp_c, percent });
        if points.len() > MAX_POINTS {
            return Err(CurveError::TooManyPoints {
                count: points.len(),
            });
        }
    }
    if points.is_empty() {
        return Err(CurveError::Empty);
    }
    Ok(points)
}

/// Percentage the curve asks for at `temp_c`, honouring the endpoints.
pub fn percent_at(points: &[CurvePoint], temp_c: i32) -> Option<i32> {
    let first = points.first()?;
    if temp_c <= first.temp_c {
        return Some(first.percent);
    }
    let mut previous = *first;
    for point in points.iter().skip(1) {
        if temp_c <= point.temp_c {
            let span = point.temp_c - previous.temp_c;
            if span == 0 {
                return Some(point.percent);
            }
            let interpolated = previous.percent
                + (temp_c - previous.temp_c) * (point.percent - previous.percent) / span;
            return Some(interpolated.clamp(0, MAX_PERCENT));
        }
        previous = *point;
    }
    points.last().map(|p| p.percent)
}

/// Raw PWM value (0..255) for `temp_c`, or `None` when the curve is unusable.
pub fn pwm_for(points: &[CurvePoint], temp_c: i32) -> Option<u8> {
    let percent = percent_at(points, temp_c)?;
    let scaled = percent * PWM_SPAN / 100;
    Some(scaled.clamp(0, PWM_SPAN) as u8)
}

pub fn is_valid(spec: &str) -> bool {
    parse(spec).is_ok()
}

pub const SILENT: &str = "20:0,35:0,40:50,55:50,70:60,85:75,95:100";
pub const BALANCED: &str = "20:0,35:0,40:50,55:60,70:75,85:90,95:100";
pub const PERFORMANCE: &str = "20:0,30:60,45:70,60:82,75:92,85:100";
pub const CUSTOM: &str = "20:0,40:50,55:60,70:75,85:90,95:100";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_curves_all_parse() {
        for spec in [SILENT, BALANCED, PERFORMANCE, CUSTOM] {
            assert!(is_valid(spec), "{spec} must stay valid");
        }
    }

    #[test]
    fn endpoints_and_interpolation_match_shell_arithmetic() {
        let points = parse(BALANCED).unwrap();
        // Below the first point the first percentage wins.
        assert_eq!(percent_at(&points, 10), Some(0));
        assert_eq!(percent_at(&points, 20), Some(0));
        // Exactly on a point.
        assert_eq!(percent_at(&points, 55), Some(60));
        // 47 C sits between 40:50 and 55:60 -> 50 + 7*10/15 = 54 (truncated).
        assert_eq!(percent_at(&points, 47), Some(54));
        // Above the last point the last percentage wins.
        assert_eq!(percent_at(&points, 150), Some(100));
        assert_eq!(pwm_for(&points, 55), Some((60u16 * 255 / 100) as u8));
    }

    #[test]
    fn descending_percentages_truncate_towards_zero() {
        let points = parse("30:80,50:20").unwrap();
        // 40 C -> 80 + 10*(20-80)/20 = 50.
        assert_eq!(percent_at(&points, 40), Some(50));
        // 35 C -> 80 + 5*(-60)/20 = 80 - 15 = 65.
        assert_eq!(percent_at(&points, 35), Some(65));
        // 33 C -> 80 + 3*(-60)/20 truncates to 80 - 9 = 71.
        assert_eq!(percent_at(&points, 33), Some(71));
    }

    #[test]
    fn negative_or_oversized_values_are_rejected() {
        assert!(matches!(
            parse("35:-10,50:80"),
            Err(CurveError::Malformed { .. })
        ));
        assert!(matches!(
            parse("35:0,160:80"),
            Err(CurveError::TempOutOfRange { temp_c: 160 })
        ));
        assert!(matches!(
            parse("35:0,50:101"),
            Err(CurveError::PercentOutOfRange { percent: 101 })
        ));
    }

    #[test]
    fn duplicate_temperatures_are_rejected() {
        assert!(matches!(
            parse("35:0,35:50,50:80"),
            Err(CurveError::NotIncreasing {
                temp_c: 35,
                previous: 35
            })
        ));
        assert!(matches!(
            parse("50:0,35:50"),
            Err(CurveError::NotIncreasing { .. })
        ));
    }

    #[test]
    fn malformed_text_is_rejected_not_silently_zeroed() {
        assert_eq!(parse(""), Err(CurveError::Empty));
        assert!(matches!(parse("   "), Err(CurveError::Empty)));
        assert!(matches!(parse("35"), Err(CurveError::Malformed { .. })));
        assert!(matches!(parse("35;50"), Err(CurveError::Malformed { .. })));
        assert!(matches!(parse("a:b"), Err(CurveError::Malformed { .. })));
        assert!(matches!(parse("3.5:50"), Err(CurveError::Malformed { .. })));
    }

    #[test]
    fn trailing_comma_and_spaces_are_tolerated() {
        assert_eq!(parse("20:0,40:50,").unwrap().len(), 2);
        assert_eq!(parse(" 20:0 , 40:50 ").unwrap().len(), 2);
    }

    #[test]
    fn pwm_output_is_clamped_to_the_byte() {
        let points = parse("0:100").unwrap();
        assert_eq!(pwm_for(&points, 25), Some(255));
        let points = parse("0:0").unwrap();
        assert_eq!(pwm_for(&points, 150), Some(0));
    }

    #[test]
    fn point_count_is_bounded() {
        let spec = (0..=MAX_POINTS)
            .map(|index| format!("{}:0", index * 2))
            .collect::<Vec<_>>()
            .join(",");
        assert!(matches!(
            parse(&spec),
            Err(CurveError::TooManyPoints { .. })
        ));
    }
}
