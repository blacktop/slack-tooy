/// Map Slack emoji short names to Unicode characters.
/// Covers the most common Slack reactions.
pub fn lookup(name: &str) -> Option<&'static str> {
    // Skin tone suffixes: strip them for lookup
    let base = name
        .strip_suffix("::skin-tone-2")
        .or_else(|| name.strip_suffix("::skin-tone-3"))
        .or_else(|| name.strip_suffix("::skin-tone-4"))
        .or_else(|| name.strip_suffix("::skin-tone-5"))
        .or_else(|| name.strip_suffix("::skin-tone-6"))
        .unwrap_or(name);

    EMOJI_MAP
        .binary_search_by_key(&base, |&(k, _)| k)
        .ok()
        .map(|i| EMOJI_MAP[i].1)
}

/// Sorted by name for binary search.
static EMOJI_MAP: &[(&str, &str)] = &[
    ("+1", "\u{1F44D}"),
    ("-1", "\u{1F44E}"),
    ("100", "\u{1F4AF}"),
    ("alien", "\u{1F47D}"),
    ("angel", "\u{1F47C}"),
    ("angry", "\u{1F620}"),
    ("anguished", "\u{1F627}"),
    ("apple", "\u{1F34E}"),
    ("astonished", "\u{1F632}"),
    ("avocado", "\u{1F951}"),
    ("banana", "\u{1F34C}"),
    ("beer", "\u{1F37A}"),
    ("beers", "\u{1F37B}"),
    ("blush", "\u{1F60A}"),
    ("bomb", "\u{1F4A3}"),
    ("bone", "\u{1F9B4}"),
    ("boom", "\u{1F4A5}"),
    ("bow", "\u{1F647}"),
    ("brain", "\u{1F9E0}"),
    ("broken_heart", "\u{1F494}"),
    ("bug", "\u{1F41B}"),
    ("bulb", "\u{1F4A1}"),
    ("burrito", "\u{1F32F}"),
    ("cactus", "\u{1F335}"),
    ("cake", "\u{1F370}"),
    ("camera", "\u{1F4F7}"),
    ("cat", "\u{1F431}"),
    ("champagne", "\u{1F37E}"),
    ("chart_with_upwards_trend", "\u{1F4C8}"),
    ("check", "\u{2714}\u{FE0F}"),
    ("checkered_flag", "\u{1F3C1}"),
    ("cherry_blossom", "\u{1F338}"),
    ("clap", "\u{1F44F}"),
    ("clown_face", "\u{1F921}"),
    ("coffee", "\u{2615}"),
    ("cold_sweat", "\u{1F630}"),
    ("confetti_ball", "\u{1F38A}"),
    ("confused", "\u{1F615}"),
    ("cool", "\u{1F192}"),
    ("cowboy_hat_face", "\u{1F920}"),
    ("cry", "\u{1F622}"),
    ("crying_cat_face", "\u{1F63F}"),
    ("dancer", "\u{1F483}"),
    ("dart", "\u{1F3AF}"),
    ("disappointed", "\u{1F61E}"),
    ("dizzy", "\u{1F4AB}"),
    ("dog", "\u{1F436}"),
    ("dollar", "\u{1F4B5}"),
    ("done", "\u{2705}"),
    ("earth_americas", "\u{1F30E}"),
    ("eggplant", "\u{1F346}"),
    ("exploding_head", "\u{1F92F}"),
    ("expressionless", "\u{1F611}"),
    ("eyes", "\u{1F440}"),
    ("face_with_rolling_eyes", "\u{1F644}"),
    ("facepalm", "\u{1F926}"),
    ("fire", "\u{1F525}"),
    ("fist", "\u{270A}"),
    ("flex", "\u{1F4AA}"),
    ("flushed", "\u{1F633}"),
    ("fork_and_knife", "\u{1F374}"),
    ("gem", "\u{1F48E}"),
    ("ghost", "\u{1F47B}"),
    ("gift", "\u{1F381}"),
    ("grimacing", "\u{1F62C}"),
    ("grin", "\u{1F601}"),
    ("grinning", "\u{1F600}"),
    ("guitar", "\u{1F3B8}"),
    ("hamburger", "\u{1F354}"),
    ("hammer", "\u{1F528}"),
    ("handshake", "\u{1F91D}"),
    ("hankey", "\u{1F4A9}"),
    ("heart", "\u{2764}\u{FE0F}"),
    ("heart_eyes", "\u{1F60D}"),
    ("heart_eyes_cat", "\u{1F63B}"),
    ("heavy_check_mark", "\u{2714}\u{FE0F}"),
    ("heavy_plus_sign", "\u{2795}"),
    ("hot_pepper", "\u{1F336}\u{FE0F}"),
    ("hugging_face", "\u{1F917}"),
    ("hushed", "\u{1F62F}"),
    ("icecream", "\u{1F366}"),
    ("imp", "\u{1F47F}"),
    ("innocent", "\u{1F607}"),
    ("joy", "\u{1F602}"),
    ("joy_cat", "\u{1F639}"),
    ("key", "\u{1F511}"),
    ("kiss", "\u{1F48B}"),
    ("kissing_heart", "\u{1F618}"),
    ("laughing", "\u{1F606}"),
    ("lemon", "\u{1F34B}"),
    ("lock", "\u{1F512}"),
    ("lollipop", "\u{1F36D}"),
    ("loudspeaker", "\u{1F4E2}"),
    ("mag", "\u{1F50D}"),
    ("mega", "\u{1F4E3}"),
    ("memo", "\u{1F4DD}"),
    ("metal", "\u{1F918}"),
    ("microphone", "\u{1F3A4}"),
    ("money_mouth_face", "\u{1F911}"),
    ("monkey_face", "\u{1F435}"),
    ("muscle", "\u{1F4AA}"),
    ("nerd_face", "\u{1F913}"),
    ("neutral_face", "\u{1F610}"),
    ("no_entry", "\u{26D4}"),
    ("no_good", "\u{1F645}"),
    ("ok", "\u{1F44C}"),
    ("ok_hand", "\u{1F44C}"),
    ("open_mouth", "\u{1F62E}"),
    ("palm_tree", "\u{1F334}"),
    ("party_popper", "\u{1F389}"),
    ("peach", "\u{1F351}"),
    ("pensive", "\u{1F614}"),
    ("persevere", "\u{1F623}"),
    ("pig", "\u{1F437}"),
    ("pizza", "\u{1F355}"),
    ("point_down", "\u{1F447}"),
    ("point_left", "\u{1F448}"),
    ("point_right", "\u{1F449}"),
    ("point_up", "\u{261D}\u{FE0F}"),
    ("point_up_2", "\u{1F446}"),
    ("poop", "\u{1F4A9}"),
    ("popcorn", "\u{1F37F}"),
    ("pray", "\u{1F64F}"),
    ("rainbow", "\u{1F308}"),
    ("raised_hands", "\u{1F64C}"),
    ("relaxed", "\u{263A}\u{FE0F}"),
    ("relieved", "\u{1F60C}"),
    ("robot_face", "\u{1F916}"),
    ("rocket", "\u{1F680}"),
    ("rofl", "\u{1F923}"),
    ("rolling_on_the_floor_laughing", "\u{1F923}"),
    ("rose", "\u{1F339}"),
    ("rotating_light", "\u{1F6A8}"),
    ("round_pushpin", "\u{1F4CD}"),
    ("saluting_face", "\u{1FAE1}"),
    ("santa", "\u{1F385}"),
    ("see_no_evil", "\u{1F648}"),
    ("shrug", "\u{1F937}"),
    ("skull", "\u{1F480}"),
    ("sleeping", "\u{1F634}"),
    ("slightly_frowning_face", "\u{1F641}"),
    ("slightly_smiling_face", "\u{1F642}"),
    ("smile", "\u{1F604}"),
    ("smiley", "\u{1F603}"),
    ("smiling_face_with_3_hearts", "\u{1F970}"),
    ("smiling_imp", "\u{1F608}"),
    ("smirk", "\u{1F60F}"),
    ("sneezing_face", "\u{1F927}"),
    ("sob", "\u{1F62D}"),
    ("sparkle", "\u{2728}"),
    ("sparkles", "\u{2728}"),
    ("star", "\u{2B50}"),
    ("star-struck", "\u{1F929}"),
    ("star2", "\u{1F31F}"),
    ("stuck_out_tongue", "\u{1F61B}"),
    ("stuck_out_tongue_closed_eyes", "\u{1F61D}"),
    ("stuck_out_tongue_winking_eye", "\u{1F61C}"),
    ("sunglasses", "\u{1F60E}"),
    ("sweat", "\u{1F613}"),
    ("sweat_drops", "\u{1F4A6}"),
    ("sweat_smile", "\u{1F605}"),
    ("taco", "\u{1F32E}"),
    ("tada", "\u{1F389}"),
    ("technologist", "\u{1F9D1}\u{200D}\u{1F4BB}"),
    ("thinking_face", "\u{1F914}"),
    ("thumbsdown", "\u{1F44E}"),
    ("thumbsup", "\u{1F44D}"),
    ("tired_face", "\u{1F62B}"),
    ("tongue", "\u{1F445}"),
    ("trophy", "\u{1F3C6}"),
    ("truck", "\u{1F69A}"),
    ("unamused", "\u{1F612}"),
    ("upside_down_face", "\u{1F643}"),
    ("v", "\u{270C}\u{FE0F}"),
    ("wave", "\u{1F44B}"),
    ("waving_hand", "\u{1F44B}"),
    ("weary", "\u{1F629}"),
    ("white_check_mark", "\u{2705}"),
    ("white_large_square", "\u{2B1C}"),
    ("wink", "\u{1F609}"),
    ("worried", "\u{1F61F}"),
    ("wrench", "\u{1F527}"),
    ("x", "\u{274C}"),
    ("yum", "\u{1F60B}"),
    ("zany_face", "\u{1F92A}"),
    ("zap", "\u{26A1}"),
    ("zipper_mouth_face", "\u{1F910}"),
    ("zzz", "\u{1F4A4}"),
];

#[cfg(test)]
mod tests {
    use super::{EMOJI_MAP, lookup};

    /// `lookup` binary-searches, which silently returns None for any
    /// entry that is out of order — enforce the precondition.
    #[test]
    fn emoji_map_is_strictly_sorted() {
        for pair in EMOJI_MAP.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "EMOJI_MAP out of order: {:?} >= {:?}",
                pair[0].0,
                pair[1].0
            );
        }
    }

    #[test]
    fn lookup_finds_every_entry() {
        for (name, unicode) in EMOJI_MAP {
            assert_eq!(lookup(name), Some(*unicode), "lookup missed {name}");
        }
    }

    #[test]
    fn lookup_strips_skin_tone_suffix() {
        assert_eq!(lookup("+1::skin-tone-4"), Some("\u{1F44D}"));
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert_eq!(lookup("definitely_not_an_emoji"), None);
    }
}
