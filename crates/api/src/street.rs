//! Street-name matching across sources that spell the same street differently:
//! CAD "N FOURTH ST", OpenStreetMap "North 4th Street", Census "N 4TH ST".

/// Upper-case words with ordinals as numbers and long forms abbreviated, nothing dropped:
/// `"North 4th Street"` gives `["N", "4TH", "ST"]`.
pub fn words(name: &str) -> Vec<String> {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                ' '
            }
        })
        .collect();
    let raw: Vec<&str> = cleaned.split_whitespace().collect();
    let mut words = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        // Multi-word forms first.
        if raw[i..].starts_with(&["FARM", "TO", "MARKET"]) {
            words.push("FM".to_string());
            i += 3;
        } else if raw[i..].starts_with(&["COUNTY", "ROAD"])
            || raw[i..].starts_with(&["COUNTY", "RD"])
        {
            words.push("CR".to_string());
            i += 2;
        } else {
            words.push(abbreviate(raw[i]).to_string());
            i += 1;
        }
    }
    words
}

/// The identifying words of a street name: [`words`] minus leading and trailing
/// directions and the street type, but never emptied.
///
/// `"N FOURTH ST"` and `"North 4th Street"` both give `["4TH"]`;
/// `"S STATE HWY 78"` and `"South State Highway 78"` both give `["STATE", "HWY", "78"]`.
pub fn core(name: &str) -> Vec<String> {
    let mut words = strip_leading_direction(&words(name)).to_vec();
    while words.len() > 1 && (is_direction(words.last().unwrap()) || is_type(words.last().unwrap()))
    {
        words.pop();
    }
    words
}

/// `words` without directions at the front, keeping at least one word.
pub fn strip_leading_direction(words: &[String]) -> &[String] {
    let mut start = 0;
    while words.len() - start > 1 && is_direction(&words[start]) {
        start += 1;
    }
    &words[start..]
}

/// Whether two street names refer to the same street.
pub fn same_street(a: &str, b: &str) -> bool {
    let (a, b) = (core(a), core(b));
    !a.is_empty() && a == b
}

fn abbreviate(word: &str) -> &str {
    match word {
        "NORTH" => "N",
        "SOUTH" => "S",
        "EAST" => "E",
        "WEST" => "W",
        "NORTHEAST" => "NE",
        "NORTHWEST" => "NW",
        "SOUTHEAST" => "SE",
        "SOUTHWEST" => "SW",
        "STREET" => "ST",
        "ROAD" => "RD",
        "DRIVE" => "DR",
        "LANE" => "LN",
        "AVENUE" => "AVE",
        "AV" => "AVE",
        "BOULEVARD" => "BLVD",
        "PARKWAY" => "PKWY",
        "COURT" => "CT",
        "CIRCLE" => "CIR",
        "TRAIL" => "TRL",
        "PLACE" => "PL",
        "TERRACE" => "TER",
        "HIGHWAY" => "HWY",
        "EXPRESSWAY" => "EXPY",
        "FREEWAY" => "FWY",
        "FIRST" => "1ST",
        "SECOND" => "2ND",
        "THIRD" => "3RD",
        "FOURTH" => "4TH",
        "FIFTH" => "5TH",
        "SIXTH" => "6TH",
        "SEVENTH" => "7TH",
        "EIGHTH" => "8TH",
        "NINTH" => "9TH",
        "TENTH" => "10TH",
        "ELEVENTH" => "11TH",
        "TWELFTH" => "12TH",
        w => w,
    }
}

fn is_direction(w: &str) -> bool {
    matches!(w, "N" | "S" | "E" | "W" | "NE" | "NW" | "SE" | "SW")
}

fn is_type(w: &str) -> bool {
    matches!(
        w,
        "ST" | "RD"
            | "DR"
            | "LN"
            | "AVE"
            | "BLVD"
            | "PKWY"
            | "CT"
            | "CIR"
            | "TRL"
            | "PL"
            | "TER"
            | "WAY"
            | "LOOP"
            | "PATH"
            | "RUN"
            | "XING"
            | "COVE"
            | "CV"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cad_and_osm_spellings_match() {
        assert!(same_street("N FOURTH ST", "North 4th Street"));
        assert!(same_street("S STATE HWY 78", "South State Highway 78"));
        assert!(same_street("W FM 6", "West FM 6"));
        assert!(same_street("BUSINESS 78", "Business 78"));
        assert!(same_street("PARKER RD", "Parker Road"));
        assert!(same_street("E PRINCETON DR", "East Princeton Drive"));
        assert!(same_street("COUNTY ROAD 976", "CR 976"));
        assert!(same_street("FARM TO MARKET 982", "FM 982"));
    }

    #[test]
    fn different_streets_do_not_match() {
        assert!(!same_street("PARKER RD", "Old Parker Road"));
        assert!(!same_street("N FOURTH ST", "North Front Street"));
        assert!(!same_street("W FM 6", "Ramble Road"));
        assert!(!same_street("", "Parker Road"));
    }

    #[test]
    fn never_empties_a_short_name() {
        assert_eq!(core("N ST"), vec!["ST"]);
    }
}
