//! Line-level diff alignment.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Same,
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub side: Side,
    pub left: String,
    pub right: String,
}

pub const MAX_LINES: usize = 400;

/// Align two texts by lines: LCS over lines, capped at the last 400 lines.
/// Returns rows where both sides always have equal counts; Left/Right rows
/// carry empty strings on the opposite side.
pub fn align(left: &str, right: &str) -> Vec<Row> {
    let left_lines: Vec<&str> = left.lines().collect();
    let right_lines: Vec<&str> = right.lines().collect();

    let left_start = left_lines.len().saturating_sub(MAX_LINES);
    let right_start = right_lines.len().saturating_sub(MAX_LINES);

    let left_capped = &left_lines[left_start..];
    let right_capped = &right_lines[right_start..];

    let lcs = longest_common_subsequence(left_capped, right_capped);
    align_with_lcs(left_capped, right_capped, &lcs)
}

fn longest_common_subsequence<'a>(left: &[&'a str], right: &[&'a str]) -> Vec<&'a str> {
    let m = left.len();
    let n = right.len();
    let mut dp = vec![vec![0; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            if left[i - 1] == right[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    let mut lcs = Vec::new();
    let (mut i, mut j) = (m, n);
    while i > 0 && j > 0 {
        if left[i - 1] == right[j - 1] {
            lcs.push(left[i - 1]);
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] > dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    lcs.reverse();
    lcs
}

fn align_with_lcs(left: &[&str], right: &[&str], lcs: &[&str]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut l_idx = 0;
    let mut r_idx = 0;
    let mut lcs_idx = 0;

    while l_idx < left.len() || r_idx < right.len() {
        let lcs_line = if lcs_idx < lcs.len() {
            Some(lcs[lcs_idx])
        } else {
            None
        };

        if let Some(common) = lcs_line
            && l_idx < left.len()
            && r_idx < right.len()
            && left[l_idx] == common
            && right[r_idx] == common
        {
            rows.push(Row {
                side: Side::Same,
                left: left[l_idx].to_string(),
                right: right[r_idx].to_string(),
            });
            l_idx += 1;
            r_idx += 1;
            lcs_idx += 1;
            continue;
        }

        if l_idx < left.len() && (lcs_line.is_none() || left[l_idx] != lcs_line.unwrap()) {
            rows.push(Row {
                side: Side::Left,
                left: left[l_idx].to_string(),
                right: String::new(),
            });
            l_idx += 1;
        } else if r_idx < right.len() {
            rows.push(Row {
                side: Side::Right,
                left: String::new(),
                right: right[r_idx].to_string(),
            });
            r_idx += 1;
        }
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_produce_all_same() {
        let text = "line 1\nline 2\nline 3";
        let rows = align(text, text);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.side == Side::Same));
        assert_eq!(rows[0].left, "line 1");
        assert_eq!(rows[0].right, "line 1");
    }

    #[test]
    fn insertion_produces_right_row_with_equal_counts() {
        let left = "line 1\nline 3";
        let right = "line 1\nline 2\nline 3";
        let rows = align(left, right);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].side, Side::Same);
        assert_eq!(rows[1].side, Side::Right);
        assert_eq!(rows[1].left, "");
        assert_eq!(rows[1].right, "line 2");
        assert_eq!(rows[2].side, Side::Same);
    }

    #[test]
    fn deletion_produces_left_row() {
        let left = "line 1\nline 2\nline 3";
        let right = "line 1\nline 3";
        let rows = align(left, right);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].side, Side::Left);
        assert_eq!(rows[1].left, "line 2");
        assert_eq!(rows[1].right, "");
    }

    #[test]
    fn cap_applies_at_400() {
        let left: String = (0..500)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let right: String = (0..500)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let rows = align(&left, &right);
        assert_eq!(rows.len(), MAX_LINES);
        assert!(rows.iter().all(|r| r.side == Side::Same));
    }

    #[test]
    fn interleaved_unique_lines_stay_aligned() {
        let left = "a\nb\nc\nd";
        let right = "a\nx\nc\ny";
        let rows = align(left, right);
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0].side, Side::Same);
        assert_eq!(rows[0].left, "a");
        assert_eq!(rows[1].side, Side::Left);
        assert_eq!(rows[1].left, "b");
        assert_eq!(rows[2].side, Side::Right);
        assert_eq!(rows[2].right, "x");
        assert_eq!(rows[3].side, Side::Same);
        assert_eq!(rows[3].left, "c");
        assert_eq!(rows[4].side, Side::Left);
        assert_eq!(rows[4].left, "d");
        assert_eq!(rows[5].side, Side::Right);
        assert_eq!(rows[5].right, "y");
    }
}
