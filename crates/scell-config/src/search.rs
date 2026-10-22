//! 主机的模糊搜索。
//!
//! 查询按空白拆成多个词，每个词都要在名称、标签、地址（`user@host:port`）或分组之一中按顺序出现（可以不连续）。
//! 连续出现优于零散出现，词首命中优于词中，名称命中优于标签、地址与分组。

use crate::HostProfile;

/// 按匹配程度从高到低返回命中的主机；分数相同时收藏在前，其余保持原顺序。
pub fn search_hosts<'a>(hosts: &'a [HostProfile], query: &str) -> Vec<&'a HostProfile> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut hits: Vec<(i32, &HostProfile)> = hosts
        .iter()
        .filter_map(|h| score(h, &terms).map(|s| (s, h)))
        .collect();
    hits.sort_by(|(a, ha), (b, hb)| b.cmp(a).then(hb.favorite.cmp(&ha.favorite)));
    hits.into_iter().map(|(_, h)| h).collect()
}

fn score(host: &HostProfile, terms: &[String]) -> Option<i32> {
    let address = host.address();
    let mut fields: Vec<(&str, i32)> = vec![(&host.name, 300), (&address, 100)];
    fields.extend(host.tags.iter().map(|t| (t.as_str(), 200)));
    fields.extend(host.group.as_deref().map(|g| (g, 0)));
    terms.iter().try_fold(0, |total, term| {
        let best = fields
            .iter()
            .filter_map(|(text, weight)| fuzzy(term, text).map(|s| s + weight))
            .max()?;
        Some(total + best)
    })
}

/// `needle` 已转小写。完整子串命中得分远高于零散的子序列命中。
fn fuzzy(needle: &str, haystack: &str) -> Option<i32> {
    let hay: Vec<char> = haystack.to_lowercase().chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return Some(0);
    }
    let boundary = |i: usize| i == 0 || !hay[i - 1].is_alphanumeric();
    if let Some(pos) = hay
        .windows(needle.len())
        .position(|w| w == needle.as_slice())
    {
        let bonus = if boundary(pos) { 200 } else { 0 };
        return Some(1000 + bonus - pos.min(100) as i32);
    }
    let (mut score, mut matched, mut last) = (0, 0, None);
    for (i, &c) in hay.iter().enumerate() {
        if matched < needle.len() && c == needle[matched] {
            score += 10;
            if boundary(i) {
                score += 15;
            }
            if last.is_some_and(|l| l + 1 == i) {
                score += 10;
            }
            last = Some(i);
            matched += 1;
        }
    }
    (matched == needle.len()).then_some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, addr: &str, tags: &[&str], group: Option<&str>) -> HostProfile {
        let mut h = HostProfile::new(name, addr);
        h.tags = tags.iter().map(|t| (*t).to_owned()).collect();
        h.group = group.map(Into::into);
        h
    }

    fn names(hosts: &[HostProfile], q: &str) -> Vec<String> {
        search_hosts(hosts, q)
            .into_iter()
            .map(|h| h.name.clone())
            .collect()
    }

    #[test]
    fn ranks_substring_over_subsequence_and_name_over_other_fields() {
        let hosts = [
            host("db-prod-01", "10.0.0.5", &[], Some("生产")),
            host("web-01", "10.0.0.2", &["nginx"], Some("生产")),
            host("dev-box", "dev.example.com", &["web"], None),
        ];
        // 名称中的 web 优先于标签中的 web
        assert_eq!(names(&hosts, "web"), ["web-01", "dev-box"]);
        // 子序列：dp1 → db-prod-01
        assert_eq!(names(&hosts, "dp1"), ["db-prod-01"]);
        // 多个词都要命中，可落在不同字段
        assert_eq!(names(&hosts, "生产 nginx"), ["web-01"]);
        // 同分时保持原顺序
        assert_eq!(names(&hosts, "10.0.0"), ["db-prod-01", "web-01"]);
        assert!(names(&hosts, "zzz").is_empty());
    }

    #[test]
    fn empty_query_keeps_order_with_favorites_first() {
        let mut hosts = [host("a", "a", &[], None), host("b", "b", &[], None)];
        hosts[1].favorite = true;
        assert_eq!(names(&hosts, "  "), ["b", "a"]);
    }
}
