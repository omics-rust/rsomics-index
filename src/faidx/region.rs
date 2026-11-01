use rsomics_common::Result;

use crate::reference::invalid;

use super::index::Index;

pub(super) struct Region {
    pub(super) header: String,
    pub(super) record: usize,
    pub(super) start: u64,
    pub(super) end: u64,
}

pub(super) fn resolve(index: &Index, request: String) -> Result<Region> {
    let (name, coordinates) = if let Some(quoted) = request.strip_prefix('{') {
        let (name, suffix) = quoted
            .split_once('}')
            .ok_or_else(|| invalid("unclosed reference-name braces"))?;
        let coordinates = if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or_else(|| invalid("unexpected text after reference-name braces"))?,
            )
        };
        (name, coordinates)
    } else if index.names.contains_key(&request) {
        if let Some((prefix, _)) = request.rsplit_once(':')
            && index.names.contains_key(prefix)
        {
            return Err(invalid(
                "ambiguous reference name; disambiguate with braces",
            ));
        }
        (request.as_str(), None)
    } else {
        let (name, coordinates) = request
            .rsplit_once(':')
            .map_or((request.as_str(), None), |(name, range)| {
                (name, Some(range))
            });
        (name, coordinates)
    };
    let record = *index
        .names
        .get(name)
        .ok_or_else(|| invalid(format!("reference not found: {name}")))?;
    let length = index.records[record].length;
    let (start, end) = if let Some(text) = coordinates {
        if let Some((first, last)) = text.split_once('-') {
            let first = if first.is_empty() {
                1
            } else {
                positive(first)?
            };
            let end = if last.is_empty() {
                length
            } else {
                positive(last)?
            };
            if !last.is_empty() && first > end {
                return Err(invalid("region start exceeds end"));
            }
            (first - 1, end)
        } else {
            (positive(text)? - 1, length)
        }
    } else {
        (0, length)
    };
    if start > length || end > length {
        eprintln!("warning: truncating region {request} to its reference length");
    }
    let end = end.min(length);
    Ok(Region {
        header: request,
        record,
        start: start.min(end),
        end,
    })
}

fn positive(text: &str) -> Result<u64> {
    if text.is_empty()
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b',')
    {
        return Err(invalid(
            "region coordinates must be positive decimal integers",
        ));
    }
    let value: u64 = text.replace(',', "").parse()?;
    if value == 0 {
        return Err(invalid(
            "region coordinates are one-based and must be positive",
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::faidx::index::Record;

    #[test]
    fn checked_regions_disambiguate_names_and_preserve_open_ends() {
        let records: Vec<_> = [("ref", 20), ("ref:2-3", 7), ("large", 5000)]
            .into_iter()
            .map(|(name, length)| Record {
                name: name.to_owned(),
                length,
                offset: 0,
                line_bases: 5,
                line_width: 6,
            })
            .collect();
        let names = records
            .iter()
            .enumerate()
            .map(|(id, record)| (record.name.clone(), id))
            .collect();
        let index = Index { records, names };
        for (request, record, start, end) in [
            ("ref", 0, 0, 20),
            ("ref:3", 0, 2, 20),
            ("ref:-3", 0, 0, 3),
            ("ref:3-", 0, 2, 20),
            ("ref:999-", 0, 20, 20),
            ("{ref}:2-3", 0, 1, 3),
            ("{ref:2-3}", 1, 0, 7),
            ("large:1,000-2,000", 2, 999, 2000),
        ] {
            let region = resolve(&index, request.to_owned()).unwrap();
            assert_eq!(
                (region.record, region.start, region.end),
                (record, start, end),
                "{request}"
            );
            assert_eq!(region.header, request);
        }
        for request in [
            "ref:2-3",
            "ref:0-1",
            "ref:3-2",
            "ref:18446744073709551616",
            "{ref",
        ] {
            assert!(resolve(&index, request.to_owned()).is_err(), "{request}");
        }
    }
}
