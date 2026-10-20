//! Invented word lists for the synthetic catalogue.
//!
//! Everything here is made up: no real channel, provider, brand or person names.

pub(crate) const BRANDS: &[&str] = &[
    "Northwind",
    "Cobalt",
    "Amberline",
    "Silverbay",
    "Bluefin",
    "Redwood",
    "Harborlight",
    "Summitview",
    "Granite",
    "Juniper",
    "Pinecrest",
    "Starling",
    "Orbitron",
    "Copperfield",
    "Driftwood",
    "Evergreen",
    "Falconridge",
    "Glacier",
    "Hollowbrook",
    "Ironleaf",
    "Kestrel",
    "Larkspur",
    "Moonstone",
    "Nimbus",
    "Oakhaven",
    "Ravenmoor",
    "Saffron",
    "Tidewater",
    "Umberly",
    "Willowmere",
    "Zephyr",
    "Brightwater",
    "Cinderfall",
    "Duskhaven",
    "Emberly",
];

pub(crate) const NEWS_SUFFIX: &[&str] = &[
    "News",
    "News 24",
    "Business",
    "World",
    "Headlines",
    "Info",
    "Weather",
    "Politics",
    "Live",
];
pub(crate) const SPORTS_SUFFIX: &[&str] = &[
    "Sports",
    "Sports 2",
    "Sports 3",
    "Arena",
    "Racing",
    "Football",
    "Golf",
    "Tennis",
    "Extreme",
    "Motors",
    "Fight Night",
];
pub(crate) const KIDS_SUFFIX: &[&str] = &[
    "Kids",
    "Junior",
    "Toons",
    "Playtime",
    "Tiny Tales",
    "Cartoons",
];
pub(crate) const DOC_SUFFIX: &[&str] = &[
    "Nature",
    "Science",
    "Explorer",
    "Wild",
    "Odyssey",
    "Earth",
    "Ocean",
    "Past & Present",
];
pub(crate) const MUSIC_SUFFIX: &[&str] = &[
    "Music", "Hits", "Rock", "Jazz", "Classics", "Beats", "Country", "Dance",
];
pub(crate) const ENT_SUFFIX: &[&str] = &[
    "One", "Two", "Plus", "Life", "Comedy", "Drama", "Family", "Gold", "Reality", "Showcase",
];
pub(crate) const VARIETY_SUFFIX: &[&str] = &[
    "Variety",
    "Culture",
    "Lifestyle",
    "Travel",
    "Food",
    "Home",
    "Style",
    "Living",
    "Global",
    "International",
];

pub(crate) const ADULT_CHANNELS: &[&str] = &[
    "Velvet Night",
    "Midnight Lounge",
    "After Dark One",
    "Crimson Hour",
    "Late Lounge Plus",
    "Moonlight Club",
    "Night Owl Adult",
    "Red Room 18+",
];
pub(crate) const SUSSEX_CHANNELS: &[&str] = &[
    "Sussex Coast TV",
    "Downs Valley TV",
    "Seven Cliffs Radio",
    "Shoreline Sussex",
    "Weald Community TV",
    "Harbour Lights FM",
    "Sussex Weather Now",
    "Chalk Hill TV",
    "Pebble Beach Radio",
    "Riverside Sussex",
];
pub(crate) const ESSEX_CHANNELS: &[&str] = &[
    "Estuary FM",
    "Marshland Radio",
    "Essex Coast FM",
    "Saltmarsh FM",
    "Pier Head FM",
    "Riverside Essex FM",
    "Lighthouse FM",
    "Creekside FM",
    "Mudflat Radio",
    "Essex Evening FM",
];
pub(crate) const GREEK_CHANNELS: &[&str] = &[
    "Κύμα TV",
    "Φάρος TV",
    "Ελιά Channel",
    "Ορίζοντας",
    "Αιγαίο Ειδήσεις",
    "Πήγασος Σπορ",
    "Κυκλάδες TV",
    "Μέλισσα Kids",
    "Ουρανός Μουσική",
    "Λαβύρινθος Ντοκιμαντέρ",
    "Ανατολή TV",
    "Θάλασσα HD",
    "Πλάτανος TV",
    "Δελφίνι Παιδικά",
];
pub(crate) const JAPANESE_CHANNELS: &[&str] = &[
    "さくらテレビ",
    "青空ニュース",
    "富士スポーツ",
    "月光シネマ",
    "海風チャンネル",
    "銀河アニメ",
    "竹林ドキュメンタリー",
    "星空ミュージック",
    "紅葉テレビ",
    "雷鳴スポーツ2",
    "小川キッズ",
    "山彦ラジオ",
    "流れ星ドラマ",
    "灯台ニュース24",
];
pub(crate) const EMOJI_CHANNELS: &[&str] = &[
    "🎬 Reel One",
    "🍿 Popcorn Premiere",
    "⭐ Star Picks HD",
    "🎞️ Classic Frames",
    "🌙 Late Show Cinema",
    "🔥 Action Blaze",
    "😂 Laugh Track",
    "👻 Fright Night",
    "❤️ Romance Reel",
    "🚀 Sci-Fi Orbit",
    "🐉 Fantasy Realm",
    "🎭 Stage & Screen",
    "🌍 World Cinema",
    "🏆 Award Winners",
    "🎵 Musicals",
    "🧸 Family Matinee",
];

/// Edge-case channel names placed first in the long-named category.
pub(crate) const VARIETY_SPECIALS: &[&str] = &[
    "Amberline \"Quoted\" & <Bracketed>, Comma TV",
    "Northwind International Culture & Lifestyle Channel — Extended Feed With An Exceptionally \
     Long Name That Keeps Going To Test Truncation, Ellipsis And Wrapping In Every Layout (FHD)",
    "  Leading And Trailing Spaces  ",
    "Ünïcödé Çhännél",
    "123 Numbers First",
];

pub(crate) const LONG_CATEGORY_NAME: &str = "International Variety, Culture, Lifestyle and General \
     Interest Channels From Every Continent — Backup Feeds (HD / FHD / UHD) [Updated Weekly]";

// ---- People ---------------------------------------------------------------------------

pub(crate) const FIRST_NAMES: &[&str] = &[
    "Mara", "Tobias", "Ilse", "Corwin", "Anouk", "Dario", "Elsbeth", "Fintan", "Greer", "Hollis",
    "Ines", "Jory", "Kesia", "Lucan", "Mireille", "Niall", "Odile", "Pascoe", "Quilla", "Rafferty",
    "Sabine", "Thaddeus", "Ulla", "Vesna", "Wren", "Ysolde", "Zeno",
];
pub(crate) const LAST_NAMES: &[&str] = &[
    "Quillfeather",
    "Ashgrove",
    "Brindlewood",
    "Cauldwell",
    "Dunmarrow",
    "Elderwick",
    "Fennimore",
    "Gallowmere",
    "Harrowgate",
    "Inchcombe",
    "Juniperhill",
    "Kettleby",
    "Larkwhistle",
    "Marrowfield",
    "Nettlecombe",
    "Oakenshaw",
    "Pembleton",
    "Quarrington",
    "Rookwood",
    "Saltonstall",
    "Thistlewaite",
    "Underhay",
    "Vellacott",
    "Yarrowby",
    "Zennorick",
];

// ---- Movies ---------------------------------------------------------------------------

pub(crate) const MOVIE_ADJ: &[&str] = &[
    "Silent",
    "Crimson",
    "Copper",
    "Velvet",
    "Paper",
    "Glass",
    "Hollow",
    "Wandering",
    "Electric",
    "Frozen",
    "Distant",
    "Midnight",
    "Quiet",
    "Endless",
    "Amber",
    "Restless",
    "Lunar",
    "Tangled",
];
pub(crate) const MOVIE_NOUN: &[&str] = &[
    "Harbor",
    "Algorithm",
    "Orchard",
    "Signal",
    "Lantern",
    "Archive",
    "Carousel",
    "Telescope",
    "Cipher",
    "Monsoon",
    "Engine",
    "Canyon",
    "Lighthouse",
    "Ledger",
    "Aviary",
    "Quarry",
    "Metronome",
    "Greenhouse",
];

pub(crate) const MOVIE_SPECIALS: &[&str] = &[
    "Ampersand & Sons: \"Quotes\" <Edition>",
    "The Remarkably Long And Winding Title Of A Film That Nobody Can Fit On A Single Poster \
     Line Without Truncating It Somewhere Around Here",
    "星の地図",
    "🚀 Rocket Garden",
    "Ça Va Être Long: Un Été à Paris",
];

pub(crate) const GREEK_MOVIES: &[&str] = &[
    "Το Καλοκαίρι του Φάρου",
    "Η Σιωπηλή Θάλασσα",
    "Δρόμοι από Αλάτι",
    "Ο Τελευταίος Ψαράς",
    "Νύχτες στην Πλατεία",
    "Το Νησί των Ανέμων",
    "Μια Βαλίτσα Όνειρα",
    "Πέτρινο Σπίτι",
];

pub(crate) const PLOT_SUBJECTS: &[&str] = &[
    "A retired cartographer",
    "Two estranged siblings",
    "A night-shift radio host",
    "An apprentice clockmaker",
    "A stubborn lighthouse keeper",
    "A team of amateur astronomers",
    "A small-town archivist",
    "A runaway courier",
];
pub(crate) const PLOT_VERBS: &[&str] = &[
    "uncovers",
    "races to decode",
    "reluctantly guards",
    "stumbles upon",
    "sets out to rebuild",
    "must deliver",
];
pub(crate) const PLOT_OBJECTS: &[&str] = &[
    "a map that redraws itself every night",
    "a signal hidden in an old broadcast",
    "the last working engine in the valley",
    "a ledger full of impossible dates",
    "a greenhouse that blooms in winter",
    "a song nobody remembers writing",
];
pub(crate) const PLOT_TWISTS: &[&str] = &[
    "Nothing is quite what it seems.",
    "Old friendships are tested along the way.",
    "Time is running out faster than expected.",
    "The truth turns out to be much closer to home.",
    "Every answer raises two more questions.",
];

// ---- Series ---------------------------------------------------------------------------

pub(crate) const SERIES_NOUN: &[&str] = &[
    "Station",
    "Files",
    "Nights",
    "Harbour",
    "Academy",
    "Chronicles",
    "Junction",
    "Protocol",
    "Affair",
    "Diaries",
    "Crossing",
    "Society",
];
pub(crate) const ANIME_SERIES: &[&str] = &["星屑の旅路", "風の剣士", "月面ガーデン"];
pub(crate) const EPISODE_TITLES: &[&str] = &[
    "The Long Night",
    "Crossroads",
    "Paper Trails",
    "Old Friends",
    "The Signal",
    "Echoes",
    "Homecoming",
    "Loose Ends",
    "Lanterns",
    "Fault Lines",
    "Second Chances",
    "Undertow",
    "Final Call",
    "Static",
    "Low Tide",
];

// ---- EPG programme titles per theme ----------------------------------------------------

pub(crate) const EPG_NEWS: &[&str] = &[
    "Morning Briefing",
    "World Desk",
    "Business Today",
    "Evening Bulletin",
    "Weather Watch",
    "Late Edition",
    "Headlines Now",
    "The Debate Room",
];
pub(crate) const EPG_SPORTS: &[&str] = &[
    "Matchday Live",
    "Goal Rush",
    "Training Ground",
    "Racing Weekend",
    "The Final Whistle",
    "Rally Highlights",
    "Courtside",
];
pub(crate) const EPG_KIDS: &[&str] = &[
    "Puzzle Pals",
    "Robo Garden",
    "The Paper Kites",
    "Moonbeam Tales",
    "Busy Beavers",
];
pub(crate) const EPG_DOC: &[&str] = &[
    "Deep Ocean Worlds",
    "Engines of the Past",
    "Wild Plateau",
    "Secrets of the Glacier",
    "Tiny Giants",
];
pub(crate) const EPG_MUSIC: &[&str] = &[
    "Top Twenty Countdown",
    "Acoustic Sessions",
    "Night Mix",
    "Retro Rewind",
    "Live From The Loft",
];
pub(crate) const EPG_ENT: &[&str] = &[
    "Quiz Champions",
    "Kitchen Duel",
    "The Sunday Show",
    "Home Rescue",
    "Garden Makeover",
    "Late Laughs",
];
pub(crate) const EPG_ADULT: &[&str] = &["Late Lounge", "After Hours", "Velvet Hour"];
pub(crate) const EPG_LOCAL: &[&str] = &[
    "Local Roundup",
    "Coastal Weather",
    "Community Hour",
    "Drive Time",
    "Market Day",
];
pub(crate) const EPG_GREEK: &[&str] = &[
    "Πρωινή Ενημέρωση",
    "Δελτίο Ειδήσεων",
    "Ταξίδια στο Αιγαίο",
    "Βραδινή Ταινία",
];
pub(crate) const EPG_JAPANESE: &[&str] = &["朝のニュース", "旅の時間", "料理の達人", "夜のドラマ"];
pub(crate) const EPG_EMOJI: &[&str] = &[
    "🎬 Premiere Night",
    "🍿 Double Feature",
    "⭐ Star Picks",
    "🎞️ Matinee Classics",
];
pub(crate) const EPG_VARIETY: &[&str] = &[
    "Global Variety Hour",
    "Street Food Stories",
    "Design & Living",
    "Weekend Escapes",
    "Culture Club",
];
