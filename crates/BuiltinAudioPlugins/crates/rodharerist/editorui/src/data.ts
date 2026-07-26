export type CategoryId =
  | "dyn"
  | "comp"
  | "wah"
  | "dist"
  | "amp"
  | "eq"
  | "mod"
  | "delay"
  | "verb"
  | "cab";

export type Category = {
  name: string;
  short: string;
  color: string;
  rgb: string;
  node: string;
};

export type Preset = {
  id: string;
  name: string;
  category: CategoryId;
  model: string;
  values: Record<string, number>;
  /** Stages in the signal path (Helix order). Empty = empty path. */
  path?: CategoryId[];
  /** Stages bypassed (off) when loaded. */
  bypassed?: CategoryId[];
};

export type Model = {
  id: string;
  name: string;
  /** Compact label for Path blocks */
  short: string;
  sub: string;
};

export type Param = {
  id: string;
  name: string;
  min: number;
  max: number;
  val: number;
  unit: string;
};

export const presetsData: Preset[] = [
  {
    id: "00A",
    name: "Empty",
    category: "amp",
    model: "mandarin",
    values: {},
    path: [],
    bypassed: ["dyn", "comp", "wah", "dist", "amp", "eq", "mod", "delay", "verb", "cab"],
  },
  {
    id: "01A",
    name: "Twin Sparkle",
    category: "amp",
    model: "twin",
    values: { amp_gain: 2.5, amp_bass: 4.5, amp_middle: 5.0, amp_treble: 6.5, amp_presence: 5.0, amp_master: 8.5 },
  },
  {
    id: "02B",
    name: "Dark & Lush",
    category: "verb",
    model: "plate",
    values: { reverb_decay: 8.5 },
  },
  {
    id: "02C",
    name: "Tripod Gallop",
    category: "dist",
    model: "screamer",
    values: { drive_gain: 7.5 },
  },
  {
    id: "02D",
    name: "SC Small Lead",
    category: "delay",
    model: "tape",
    values: { delay_mix: 30 },
  },
  {
    id: "03A",
    name: "Wild Year",
    category: "mod",
    model: "chorus",
    values: { chorus_mix: 40 },
  },
  {
    id: "04A",
    name: "Rats Nest Riff",
    category: "dist",
    model: "rat",
    values: { drive_gain: 8.0, drive_tone: 4.0 },
  },
  {
    id: "05A",
    name: "Chime Room",
    category: "amp",
    model: "topboost",
    values: { amp_gain: 5.5, amp_middle: 3.5, amp_treble: 7.0, amp_presence: 6.5 },
  },
  {
    id: "06D",
    name: "Mandarine Gaze",
    category: "amp",
    model: "mandarin",
    values: {
      amp_gain: 5.0,
      amp_bass: 5.0,
      amp_middle: 5.5,
      amp_treble: 5.0,
      amp_presence: 5.0,
      amp_master: 5.0,
    },
  },
  {
    id: "07A",
    name: "Modern Crush",
    category: "amp",
    model: "recto",
    values: { amp_gain: 7.5, amp_bass: 6.0, amp_middle: 3.0, amp_treble: 5.5, amp_presence: 6.0, amp_master: 6.5 },
  },
  {
    id: "08B",
    name: "Electric Version",
    category: "amp",
    model: "plexi",
    values: { amp_gain: 7.5, amp_bass: 4.0, amp_middle: 6.2 },
  },
  {
    id: "08C",
    name: "JCM Stack",
    category: "amp",
    model: "jcm",
    values: { amp_gain: 7.0, amp_middle: 6.5, amp_treble: 5.5, amp_presence: 5.5 },
  },
  {
    id: "09A",
    name: "Funk Clean",
    category: "mod",
    model: "chorus",
    values: { chorus_mix: 60 },
  },
  {
    id: "10A",
    name: "Slate Solo",
    category: "amp",
    model: "slate",
    values: { amp_gain: 6.5, amp_bass: 5.5, amp_middle: 5.5, amp_treble: 6.0, amp_master: 6.5 },
  },
  {
    id: "11A",
    name: "Face Melt",
    category: "dist",
    model: "fuzz",
    values: { drive_gain: 9.0, drive_tone: 3.5, drive_level: 5.5 },
  },
  {
    id: "12A",
    name: "Bassman Room",
    category: "amp",
    model: "bassman",
    values: { amp_gain: 5.0, amp_bass: 7.0, amp_middle: 4.0, amp_treble: 4.5 },
  },
  {
    id: "13A",
    name: "Lam Long Sweep",
    category: "mod",
    model: "molam_swirl",
    values: { chorus_rate: 2.0, chorus_depth: 7.5, chorus_mix: 70 },
  },
  {
    id: "13B",
    name: "Phin Throb",
    category: "mod",
    model: "phin_vibe",
    values: { chorus_rate: 5.0, chorus_depth: 6.5, chorus_mix: 80 },
  },
  {
    id: "13C",
    name: "Khaen Drift",
    category: "mod",
    model: "khaen_swirl",
    values: { chorus_rate: 1.5, chorus_depth: 8.0, chorus_mix: 65 },
  },
  {
    id: "13D",
    name: "Bi-Lam Wide",
    category: "mod",
    model: "bi_lam",
    values: { chorus_rate: 1.0, chorus_depth: 8.5, chorus_mix: 60 },
  },
  {
    id: "13E",
    name: "Isan Jet Lead",
    category: "mod",
    model: "isan_jet",
    values: { chorus_rate: 7.0, chorus_depth: 7.0, chorus_mix: 70 },
  },
];

export const categories: Record<CategoryId, Category> = {
  dyn: {
    name: "Gate",
    short: "Gate",
    color: "var(--c-dyn)",
    rgb: "91, 124, 250",
    node: "gate",
  },
  comp: {
    name: "Compressor",
    short: "Comp",
    color: "var(--c-comp)",
    rgb: "240, 200, 80",
    node: "comp",
  },
  wah: {
    name: "Wah",
    short: "Wah",
    color: "var(--c-wah)",
    rgb: "170, 200, 80",
    node: "wah",
  },
  dist: {
    name: "Distortion",
    short: "Dist",
    color: "var(--c-dist)",
    rgb: "232, 148, 42",
    node: "drive",
  },
  amp: {
    name: "Amp",
    short: "Amp",
    color: "var(--c-amp)",
    rgb: "232, 92, 92",
    node: "amp",
  },
  eq: {
    name: "Equalizer",
    short: "EQ",
    color: "var(--c-eq)",
    rgb: "120, 220, 200",
    node: "eq",
  },
  mod: {
    name: "Modulation",
    short: "Mod",
    color: "var(--c-mod)",
    rgb: "61, 184, 232",
    node: "mod",
  },
  delay: {
    name: "Delay",
    short: "Delay",
    color: "var(--c-delay)",
    rgb: "61, 214, 140",
    node: "delay",
  },
  verb: {
    name: "Reverb",
    short: "Verb",
    color: "var(--c-verb)",
    rgb: "168, 120, 240",
    node: "reverb",
  },
  cab: {
    name: "Cabinet",
    short: "Cab",
    color: "var(--c-cab)",
    rgb: "224, 112, 176",
    node: "cab",
  },
};

export const models: Record<CategoryId, Model[]> = {
  dyn: [
    {
      id: "gate",
      name: "Noise Gate",
      short: "Gate",
      sub: "Dynamic threshold noise reduction",
    },
  ],
  comp: [
    {
      id: "softknee",
      name: "Studio Comp",
      short: "Comp",
      sub: "Stereo-linked soft-knee compressor",
    },
  ],
  eq: [
    {
      id: "parametric",
      name: "Studio EQ",
      short: "EQ",
      sub: "4-band parametric tone shaping",
    },
  ],
  dist: [
    {
      id: "screamer",
      name: "Green Screamer",
      short: "Screamer",
      sub: "Tube drive mid-boost pedal",
    },
    {
      id: "minotaur",
      name: "Minotaur Boost",
      short: "Minotaur",
      sub: "Buffered analog clean boost",
    },
    {
      id: "rat",
      name: "Rats Nest",
      short: "Rat",
      sub: "Hard-clipping filthy distortion",
    },
    {
      id: "breaker",
      name: "Breaker Blues",
      short: "Breaker",
      sub: "Soft low-gain overdrive",
    },
    {
      id: "fuzz",
      name: "Face Fuzz",
      short: "Fuzz",
      sub: "Gated asymmetric fuzz",
    },
    {
      id: "centurion",
      name: "Centurion OD",
      short: "Centurion",
      sub: "Transparent mid-forward overdrive",
    },
    {
      id: "ds_one",
      name: "DS Classic",
      short: "DS-1",
      sub: "Raw orange-box hard clipper",
    },
    {
      id: "super_drive",
      name: "Super Drive",
      short: "SuperDrv",
      sub: "Asymmetric smooth overdrive",
    },
    {
      id: "metal_core",
      name: "Metal Core",
      short: "Metal",
      sub: "Huge-gain scooped metal distortion",
    },
    {
      id: "tight_rift",
      name: "Tight Rift",
      short: "Rift",
      sub: "Modern tight high-gain, djent-ready",
    },
  ],
  amp: [
    {
      id: "mandarin",
      name: "Mandarin 80",
      short: "Mandarin",
      sub: "1980 vintage British Orange tube head",
    },
    {
      id: "plexi",
      name: "Brit Plexi 100",
      short: "Plexi",
      sub: "Super Lead 1959 plexiglass Marshall",
    },
    {
      id: "twin",
      name: "Twin Clean",
      short: "Twin",
      sub: "High-headroom American clean combo",
    },
    {
      id: "topboost",
      name: "Top Boost",
      short: "TopBoost",
      sub: "Chiming British class-A combo",
    },
    {
      id: "recto",
      name: "Recto Modern",
      short: "Recto",
      sub: "Tight modern high-gain rectifier",
    },
    {
      id: "jcm",
      name: "JCM Crunch",
      short: "JCM",
      sub: "Classic British stack crunch",
    },
    {
      id: "slate",
      name: "Lead Slate",
      short: "Slate",
      sub: "Hot-rodded saturated lead amp",
    },
    {
      id: "bassman",
      name: "Bassman",
      short: "Bassman",
      sub: "Loose American bass-heavy head",
    },
    {
      id: "nam_capture",
      name: "NAM Capture",
      short: "NAM",
      sub: "Neural amp/cab capture (.nam)",
    },
    {
      id: "bypass",
      name: "Bypass",
      short: "Bypass",
      sub: "Pass the Tone/Amp slot through unprocessed",
    },
  ],
  wah: [
    {
      id: "cry_wah",
      name: "Cry Wah",
      short: "Cry",
      sub: "Pedal-position resonant sweep",
    },
    {
      id: "touch_wah",
      name: "Touch Wah",
      short: "Touch",
      sub: "Envelope-following auto wah",
    },
  ],
  mod: [
    {
      id: "chorus",
      name: "70s Analog Chorus",
      short: "Chorus",
      sub: "Warm analog modulated chorus",
    },
    {
      id: "phaser",
      name: "Vibe Phase 90",
      short: "Phaser",
      sub: "Swept 4-stage analog phaser",
    },
    {
      id: "flanger",
      name: "Jet Flanger",
      short: "Flanger",
      sub: "Short-delay jet-sweep flanger",
    },
    {
      id: "tremolo",
      name: "Opto Tremolo",
      short: "Trem",
      sub: "Amp-style optical tremolo",
    },
    {
      id: "molam_swirl",
      name: "Molam Swirl",
      short: "Molam",
      sub: "4-stage, regeneration wide open — big vowel sweep",
    },
    {
      id: "phin_vibe",
      name: "Phin Vibe",
      short: "PhinVibe",
      sub: "Staggered stages, no feedback — a throb, not a sweep",
    },
    {
      id: "khaen_swirl",
      name: "Khaen Swirl",
      short: "Khaen",
      sub: "8-stage, four notches — lush and continuous",
    },
    {
      id: "bi_lam",
      name: "Bi-Lam",
      short: "BiLam",
      sub: "12-stage cascade, slow and very wide",
    },
    {
      id: "isan_jet",
      name: "Isan Jet",
      short: "IsanJet",
      sub: "6-stage, hard regeneration — fast and narrow up top",
    },
  ],
  delay: [
    { id: "tape", name: "Tape Echo", short: "Tape", sub: "Warm saturated tape delay" },
    {
      id: "digital",
      name: "Digital Delay",
      short: "Digital",
      sub: "Clean full-bandwidth repeats",
    },
    {
      id: "analog",
      name: "Analog BBD",
      short: "Analog",
      sub: "Dark thinning bucket-brigade repeats",
    },
    {
      id: "ping_pong",
      name: "Ping Pong",
      short: "Ping",
      sub: "Repeats alternating left and right",
    },
    {
      id: "dual",
      name: "Dual Delay",
      short: "Dual",
      sub: "Quarter note left, dotted eighth right",
    },
  ],
  verb: [
    {
      id: "plate",
      name: "Studio Plate",
      short: "Plate",
      sub: "Sustained metallic plate resonance",
    },
    {
      id: "room",
      name: "Tracking Room",
      short: "Room",
      sub: "Short, damped early reflections",
    },
    {
      id: "hall",
      name: "Concert Hall",
      short: "Hall",
      sub: "Long predelay and spacious tail",
    },
    {
      id: "shimmer",
      name: "Shimmer",
      short: "Shimmer",
      sub: "Octave-up voice ascending in the tail",
    },
  ],
  cab: [
    {
      id: "vintage_cab",
      name: "1960v Vintage 4x12",
      short: "4x12",
      sub: "Celestion vintage cabinet sim",
    },
    {
      id: "american_2x12",
      name: "American 2x12",
      short: "2x12",
      sub: "Bright, tight open-back combo",
    },
    {
      id: "tweed_1x12",
      name: "Tweed 1x12",
      short: "Tweed",
      sub: "Small, boxy single-speaker combo",
    },
    {
      id: "modern_412",
      name: "Modern 4x12",
      short: "Modern",
      sub: "Tight, scooped, extended highs",
    },
    {
      id: "open_back",
      name: "Open Back",
      short: "Open",
      sub: "Wide front/rear cancellation and open dynamics",
    },
    {
      id: "vintage_212",
      name: "Vintage 2x12",
      short: "Vint 2x12",
      sub: "Warm paired speakers with damped cone breakup",
    },
    {
      id: "oversized_412",
      name: "Oversized 4x12",
      short: "Oversized",
      sub: "Deep closed-box modes and tight upper mids",
    },
    {
      id: "bass_cabinet",
      name: "Bass Cabinet",
      short: "Bass",
      sub: "Extended low-end radiation with controlled breakup",
    },
    {
      id: "brit_412",
      name: "British Stack 4x12",
      short: "Brit",
      sub: "Mid-forward greenback-voiced closed 4x12",
    },
    {
      id: "uber_412",
      name: "Uberkab 4x12",
      short: "Uber",
      sub: "Tight, scooped German high-gain 4x12",
    },
    {
      id: "slo_412",
      name: "SLO Custom 4x12",
      short: "SLO",
      sub: "Smooth, singing high-gain 4x12",
    },
    {
      id: "ir",
      name: "Impulse Response",
      short: "IR",
      sub: "Convolution with a loaded .wav cabinet IR",
    },
  ],
};

export const parameterDefaults: Record<string, Param[]> = {
  gate: [
    {
      id: "gate_thresh",
      name: "Threshold",
      min: -80,
      max: 0,
      val: -55,
      unit: "dB",
    },
  ],
  softknee: [
    { id: "comp_thresh", name: "Threshold", min: -60, max: 0, val: -24, unit: "dB" },
    { id: "comp_ratio", name: "Ratio", min: 1, max: 20, val: 2, unit: ":1" },
    { id: "comp_attack", name: "Attack", min: 0.1, max: 100, val: 10, unit: "ms" },
    { id: "comp_release", name: "Release", min: 10, max: 1000, val: 120, unit: "ms" },
    { id: "comp_makeup", name: "Makeup", min: 0, max: 24, val: 0, unit: "dB" },
  ],
  parametric: [
    { id: "eq_low_gain", name: "Low", min: -15, max: 15, val: 0, unit: "dB" },
    { id: "eq_mid1_freq", name: "Mid1 Freq", min: 100, max: 1000, val: 400, unit: "Hz" },
    { id: "eq_mid1_gain", name: "Mid1", min: -15, max: 15, val: 0, unit: "dB" },
    { id: "eq_mid2_freq", name: "Mid2 Freq", min: 600, max: 6000, val: 2000, unit: "Hz" },
    { id: "eq_mid2_gain", name: "Mid2", min: -15, max: 15, val: 0, unit: "dB" },
    { id: "eq_high_gain", name: "High", min: -15, max: 15, val: 0, unit: "dB" },
  ],
  screamer: [
    {
      id: "drive_gain",
      name: "Drive",
      min: 0,
      max: 10,
      val: 6.0,
      unit: "",
    },
    {
      id: "drive_tone",
      name: "Tone",
      min: 0,
      max: 10,
      val: 5.5,
      unit: "",
    },
    {
      id: "drive_level",
      name: "Level",
      min: 0,
      max: 10,
      val: 6.5,
      unit: "",
    },
  ],
  minotaur: [
    {
      id: "drive_gain",
      name: "Gain",
      min: 0,
      max: 10,
      val: 3.5,
      unit: "",
    },
    {
      id: "drive_tone",
      name: "Tone",
      min: 0,
      max: 10,
      val: 5.0,
      unit: "",
    },
    {
      id: "drive_level",
      name: "Output",
      min: 0,
      max: 10,
      val: 7.0,
      unit: "",
    },
  ],
  rat: [
    { id: "drive_gain", name: "Distortion", min: 0, max: 10, val: 7.5, unit: "" },
    { id: "drive_tone", name: "Filter", min: 0, max: 10, val: 4.5, unit: "" },
    { id: "drive_level", name: "Volume", min: 0, max: 10, val: 6.0, unit: "" },
  ],
  breaker: [
    { id: "drive_gain", name: "Drive", min: 0, max: 10, val: 4.5, unit: "" },
    { id: "drive_tone", name: "Tone", min: 0, max: 10, val: 5.5, unit: "" },
    { id: "drive_level", name: "Level", min: 0, max: 10, val: 6.5, unit: "" },
  ],
  fuzz: [
    { id: "drive_gain", name: "Fuzz", min: 0, max: 10, val: 8.5, unit: "" },
    { id: "drive_tone", name: "Tone", min: 0, max: 10, val: 3.5, unit: "" },
    { id: "drive_level", name: "Level", min: 0, max: 10, val: 5.5, unit: "" },
  ],
  centurion: [
    { id: "drive_gain", name: "Gain", min: 0, max: 10, val: 5.0, unit: "" },
    { id: "drive_tone", name: "Tone", min: 0, max: 10, val: 6.0, unit: "" },
    { id: "drive_level", name: "Output", min: 0, max: 10, val: 6.5, unit: "" },
  ],
  ds_one: [
    { id: "drive_gain", name: "Dist", min: 0, max: 10, val: 6.5, unit: "" },
    { id: "drive_tone", name: "Tone", min: 0, max: 10, val: 5.0, unit: "" },
    { id: "drive_level", name: "Level", min: 0, max: 10, val: 6.0, unit: "" },
  ],
  super_drive: [
    { id: "drive_gain", name: "Drive", min: 0, max: 10, val: 5.5, unit: "" },
    { id: "drive_tone", name: "Tone", min: 0, max: 10, val: 5.5, unit: "" },
    { id: "drive_level", name: "Level", min: 0, max: 10, val: 6.0, unit: "" },
  ],
  metal_core: [
    { id: "drive_gain", name: "Dist", min: 0, max: 10, val: 7.5, unit: "" },
    { id: "drive_tone", name: "Tone", min: 0, max: 10, val: 5.0, unit: "" },
    { id: "drive_level", name: "Level", min: 0, max: 10, val: 5.5, unit: "" },
  ],
  tight_rift: [
    { id: "drive_gain", name: "Gain", min: 0, max: 10, val: 7.0, unit: "" },
    { id: "drive_tone", name: "Tone", min: 0, max: 10, val: 5.5, unit: "" },
    { id: "drive_level", name: "Level", min: 0, max: 10, val: 5.5, unit: "" },
  ],
  mandarin: [
    {
      id: "amp_gain",
      name: "Drive",
      min: 0,
      max: 10,
      val: 5.0,
      unit: "",
    },
    {
      id: "amp_bass",
      name: "Bass",
      min: 0,
      max: 10,
      val: 5.0,
      unit: "",
    },
    {
      id: "amp_middle",
      name: "Mid",
      min: 0,
      max: 10,
      val: 5.5,
      unit: "",
    },
    {
      id: "amp_treble",
      name: "Treble",
      min: 0,
      max: 10,
      val: 5.0,
      unit: "",
    },
    {
      id: "amp_presence",
      name: "Presence",
      min: 0,
      max: 10,
      val: 5.0,
      unit: "",
    },
    {
      id: "amp_master",
      name: "Master",
      min: 0,
      max: 10,
      val: 5.0,
      unit: "",
    },
  ],
  plexi: [
    {
      id: "amp_gain",
      name: "Pre Gain",
      min: 0,
      max: 10,
      val: 7.5,
      unit: "",
    },
    {
      id: "amp_bass",
      name: "Bass",
      min: 0,
      max: 10,
      val: 4.0,
      unit: "",
    },
    {
      id: "amp_middle",
      name: "Middle",
      min: 0,
      max: 10,
      val: 6.2,
      unit: "",
    },
    {
      id: "amp_treble",
      name: "Treble",
      min: 0,
      max: 10,
      val: 6.5,
      unit: "",
    },
    {
      id: "amp_presence",
      name: "Presence",
      min: 0,
      max: 10,
      val: 6.0,
      unit: "",
    },
    {
      id: "amp_master",
      name: "Master",
      min: 0,
      max: 10,
      val: 5.0,
      unit: "",
    },
  ],
  twin: [
    { id: "amp_gain", name: "Drive", min: 0, max: 10, val: 2.5, unit: "" },
    { id: "amp_bass", name: "Bass", min: 0, max: 10, val: 4.5, unit: "" },
    { id: "amp_middle", name: "Mid", min: 0, max: 10, val: 5.0, unit: "" },
    { id: "amp_treble", name: "Treble", min: 0, max: 10, val: 6.5, unit: "" },
    { id: "amp_presence", name: "Presence", min: 0, max: 10, val: 5.0, unit: "" },
    { id: "amp_master", name: "Master", min: 0, max: 10, val: 8.5, unit: "" },
  ],
  topboost: [
    { id: "amp_gain", name: "Drive", min: 0, max: 10, val: 5.5, unit: "" },
    { id: "amp_bass", name: "Bass", min: 0, max: 10, val: 5.5, unit: "" },
    { id: "amp_middle", name: "Mid", min: 0, max: 10, val: 3.5, unit: "" },
    { id: "amp_treble", name: "Treble", min: 0, max: 10, val: 7.0, unit: "" },
    { id: "amp_presence", name: "Presence", min: 0, max: 10, val: 6.5, unit: "" },
    { id: "amp_master", name: "Master", min: 0, max: 10, val: 6.0, unit: "" },
  ],
  recto: [
    { id: "amp_gain", name: "Drive", min: 0, max: 10, val: 7.5, unit: "" },
    { id: "amp_bass", name: "Bass", min: 0, max: 10, val: 6.0, unit: "" },
    { id: "amp_middle", name: "Mid", min: 0, max: 10, val: 3.0, unit: "" },
    { id: "amp_treble", name: "Treble", min: 0, max: 10, val: 5.5, unit: "" },
    { id: "amp_presence", name: "Presence", min: 0, max: 10, val: 6.0, unit: "" },
    { id: "amp_master", name: "Master", min: 0, max: 10, val: 6.5, unit: "" },
  ],
  jcm: [
    { id: "amp_gain", name: "Pre Gain", min: 0, max: 10, val: 7.0, unit: "" },
    { id: "amp_bass", name: "Bass", min: 0, max: 10, val: 5.0, unit: "" },
    { id: "amp_middle", name: "Middle", min: 0, max: 10, val: 6.5, unit: "" },
    { id: "amp_treble", name: "Treble", min: 0, max: 10, val: 5.5, unit: "" },
    { id: "amp_presence", name: "Presence", min: 0, max: 10, val: 5.5, unit: "" },
    { id: "amp_master", name: "Master", min: 0, max: 10, val: 5.5, unit: "" },
  ],
  slate: [
    { id: "amp_gain", name: "Drive", min: 0, max: 10, val: 6.5, unit: "" },
    { id: "amp_bass", name: "Bass", min: 0, max: 10, val: 5.5, unit: "" },
    { id: "amp_middle", name: "Mid", min: 0, max: 10, val: 5.5, unit: "" },
    { id: "amp_treble", name: "Treble", min: 0, max: 10, val: 6.0, unit: "" },
    { id: "amp_presence", name: "Presence", min: 0, max: 10, val: 6.5, unit: "" },
    { id: "amp_master", name: "Master", min: 0, max: 10, val: 6.5, unit: "" },
  ],
  bassman: [
    { id: "amp_gain", name: "Drive", min: 0, max: 10, val: 5.0, unit: "" },
    { id: "amp_bass", name: "Bass", min: 0, max: 10, val: 7.0, unit: "" },
    { id: "amp_middle", name: "Mid", min: 0, max: 10, val: 4.0, unit: "" },
    { id: "amp_treble", name: "Treble", min: 0, max: 10, val: 4.5, unit: "" },
    { id: "amp_presence", name: "Presence", min: 0, max: 10, val: 4.5, unit: "" },
    { id: "amp_master", name: "Master", min: 0, max: 10, val: 6.0, unit: "" },
  ],
  nam_capture: [
    { id: "nam_input_trim", name: "Input Trim", min: -24, max: 24, val: 0, unit: "dB" },
    { id: "nam_output_trim", name: "Output Trim", min: -24, max: 24, val: 0, unit: "dB" },
    { id: "nam_mix", name: "Mix", min: 0, max: 100, val: 100, unit: "%" },
  ],
  bypass: [],
  ir: [],
  cry_wah: [
    { id: "wah_pos", name: "Position", min: 0, max: 10, val: 4.5, unit: "" },
    { id: "wah_res", name: "Resonance", min: 0, max: 10, val: 5.0, unit: "" },
  ],
  touch_wah: [
    { id: "wah_pos", name: "Base Freq", min: 0, max: 10, val: 2.0, unit: "" },
    { id: "wah_res", name: "Resonance", min: 0, max: 10, val: 5.0, unit: "" },
    { id: "wah_sens", name: "Sensitivity", min: 0, max: 10, val: 5.0, unit: "" },
  ],
  // Mix now reaches its deepest notch at 100% (it used to peak mid-travel and
  // fade out at the top), so these defaults sit high on purpose.
  phaser: [
    { id: "chorus_rate", name: "Rate", min: 0, max: 10, val: 3.0, unit: "" },
    { id: "chorus_depth", name: "Depth", min: 0, max: 10, val: 6.5, unit: "" },
    { id: "chorus_mix", name: "Mix", min: 0, max: 100, val: 55, unit: "%" },
  ],
  molam_swirl: [
    { id: "chorus_rate", name: "Rate", min: 0, max: 10, val: 2.2, unit: "" },
    { id: "chorus_depth", name: "Depth", min: 0, max: 10, val: 7.0, unit: "" },
    { id: "chorus_mix", name: "Mix", min: 0, max: 100, val: 65, unit: "%" },
  ],
  phin_vibe: [
    { id: "chorus_rate", name: "Rate", min: 0, max: 10, val: 4.5, unit: "" },
    { id: "chorus_depth", name: "Depth", min: 0, max: 10, val: 6.0, unit: "" },
    { id: "chorus_mix", name: "Mix", min: 0, max: 100, val: 75, unit: "%" },
  ],
  khaen_swirl: [
    { id: "chorus_rate", name: "Rate", min: 0, max: 10, val: 1.8, unit: "" },
    { id: "chorus_depth", name: "Depth", min: 0, max: 10, val: 7.5, unit: "" },
    { id: "chorus_mix", name: "Mix", min: 0, max: 100, val: 60, unit: "%" },
  ],
  bi_lam: [
    { id: "chorus_rate", name: "Rate", min: 0, max: 10, val: 1.2, unit: "" },
    { id: "chorus_depth", name: "Depth", min: 0, max: 10, val: 8.0, unit: "" },
    { id: "chorus_mix", name: "Mix", min: 0, max: 100, val: 55, unit: "%" },
  ],
  isan_jet: [
    { id: "chorus_rate", name: "Rate", min: 0, max: 10, val: 6.5, unit: "" },
    { id: "chorus_depth", name: "Depth", min: 0, max: 10, val: 6.5, unit: "" },
    { id: "chorus_mix", name: "Mix", min: 0, max: 100, val: 65, unit: "%" },
  ],
  flanger: [
    { id: "chorus_rate", name: "Rate", min: 0, max: 10, val: 2.5, unit: "" },
    { id: "chorus_depth", name: "Depth", min: 0, max: 10, val: 6.0, unit: "" },
    { id: "chorus_mix", name: "Mix", min: 0, max: 100, val: 50, unit: "%" },
  ],
  tremolo: [
    { id: "chorus_rate", name: "Rate", min: 0, max: 10, val: 4.5, unit: "" },
    { id: "chorus_depth", name: "Depth", min: 0, max: 10, val: 6.0, unit: "" },
    { id: "chorus_mix", name: "Shape", min: 0, max: 100, val: 20, unit: "%" },
  ],
  chorus: [
    {
      id: "chorus_rate",
      name: "Rate",
      min: 0,
      max: 10,
      val: 4.0,
      unit: "",
    },
    {
      id: "chorus_depth",
      name: "Depth",
      min: 0,
      max: 10,
      val: 5.5,
      unit: "",
    },
    {
      id: "chorus_mix",
      name: "Mix",
      min: 0,
      max: 100,
      val: 40,
      unit: "%",
    },
  ],
  tape: [
    {
      id: "delay_time",
      name: "Time",
      min: 40,
      max: 1200,
      val: 420,
      unit: "ms",
    },
    {
      id: "delay_fb",
      name: "Feedback",
      min: 0,
      max: 100,
      val: 35,
      unit: "%",
    },
    {
      id: "delay_mix",
      name: "Mix",
      min: 0,
      max: 100,
      val: 30,
      unit: "%",
    },
    { id: "delay_tone", name: "Tone", min: 0, max: 10, val: 5, unit: "" },
  ],
  // The Delay slot's voicings share Time/Feedback/Mix/Tone (the voicing is the
  // model, as in the Mod and Reverb slots); Tone is centred on each voicing's
  // own feedback colour, so 5 means something different on each.
  digital: [
    { id: "delay_time", name: "Time", min: 40, max: 1200, val: 380, unit: "ms" },
    { id: "delay_fb", name: "Feedback", min: 0, max: 100, val: 30, unit: "%" },
    { id: "delay_mix", name: "Mix", min: 0, max: 100, val: 28, unit: "%" },
    { id: "delay_tone", name: "Tone", min: 0, max: 10, val: 5, unit: "" },
  ],
  analog: [
    { id: "delay_time", name: "Time", min: 40, max: 1200, val: 320, unit: "ms" },
    { id: "delay_fb", name: "Feedback", min: 0, max: 100, val: 45, unit: "%" },
    { id: "delay_mix", name: "Mix", min: 0, max: 100, val: 32, unit: "%" },
    { id: "delay_tone", name: "Tone", min: 0, max: 10, val: 5, unit: "" },
  ],
  ping_pong: [
    { id: "delay_time", name: "Time", min: 40, max: 1200, val: 300, unit: "ms" },
    { id: "delay_fb", name: "Feedback", min: 0, max: 100, val: 42, unit: "%" },
    { id: "delay_mix", name: "Mix", min: 0, max: 100, val: 35, unit: "%" },
    { id: "delay_tone", name: "Tone", min: 0, max: 10, val: 5, unit: "" },
  ],
  dual: [
    { id: "delay_time", name: "Time", min: 40, max: 1200, val: 500, unit: "ms" },
    { id: "delay_fb", name: "Feedback", min: 0, max: 100, val: 38, unit: "%" },
    { id: "delay_mix", name: "Mix", min: 0, max: 100, val: 33, unit: "%" },
    { id: "delay_tone", name: "Tone", min: 0, max: 10, val: 5, unit: "" },
  ],
  plate: [
    {
      id: "reverb_decay",
      name: "Decay",
      min: 0.5,
      max: 15,
      val: 8.5,
      unit: "s",
    },
    {
      id: "reverb_mix",
      name: "Mix",
      min: 0,
      max: 100,
      val: 55,
      unit: "%",
    },
  ],
  // Room/Hall share the Decay/Mix knobs (the voicing is the model, as in the
  // Mod slot) — distinct defaults suit each space. Shimmer adds a dedicated
  // octave-up feedback amount.
  room: [
    { id: "reverb_decay", name: "Decay", min: 0.5, max: 15, val: 1.8, unit: "s" },
    { id: "reverb_mix", name: "Mix", min: 0, max: 100, val: 30, unit: "%" },
  ],
  hall: [
    { id: "reverb_decay", name: "Decay", min: 0.5, max: 15, val: 11, unit: "s" },
    { id: "reverb_mix", name: "Mix", min: 0, max: 100, val: 45, unit: "%" },
  ],
  shimmer: [
    { id: "reverb_decay", name: "Decay", min: 0.5, max: 15, val: 12, unit: "s" },
    { id: "reverb_mix", name: "Mix", min: 0, max: 100, val: 40, unit: "%" },
    { id: "reverb_shimmer", name: "Shimmer", min: 0, max: 100, val: 62, unit: "%" },
  ],
  vintage_cab: [
    { id: "cab_mic_type", name: "Mic Type", min: 0, max: 2, val: 0, unit: "" },
    { id: "cab_mic", name: "Mic Pos", min: 0, max: 100, val: 20, unit: "%" },
    { id: "cab_dist", name: "Distance", min: 0, max: 100, val: 40, unit: "%" },
  ],
  american_2x12: [
    { id: "cab_mic_type", name: "Mic Type", min: 0, max: 2, val: 0, unit: "" },
    { id: "cab_mic", name: "Mic Pos", min: 0, max: 100, val: 35, unit: "%" },
    { id: "cab_dist", name: "Distance", min: 0, max: 100, val: 30, unit: "%" },
  ],
  tweed_1x12: [
    { id: "cab_mic_type", name: "Mic Type", min: 0, max: 2, val: 1, unit: "" },
    { id: "cab_mic", name: "Mic Pos", min: 0, max: 100, val: 15, unit: "%" },
    { id: "cab_dist", name: "Distance", min: 0, max: 100, val: 55, unit: "%" },
  ],
  modern_412: [
    { id: "cab_mic_type", name: "Mic Type", min: 0, max: 2, val: 0, unit: "" },
    { id: "cab_mic", name: "Mic Pos", min: 0, max: 100, val: 45, unit: "%" },
    { id: "cab_dist", name: "Distance", min: 0, max: 100, val: 20, unit: "%" },
  ],
  open_back: [
    { id: "cab_mic_type", name: "Mic Type", min: 0, max: 2, val: 2, unit: "" },
    { id: "cab_mic", name: "Mic Pos", min: 0, max: 100, val: 55, unit: "%" },
    { id: "cab_dist", name: "Distance", min: 0, max: 100, val: 45, unit: "%" },
  ],
  vintage_212: [
    { id: "cab_mic_type", name: "Mic Type", min: 0, max: 2, val: 1, unit: "" },
    { id: "cab_mic", name: "Mic Pos", min: 0, max: 100, val: 40, unit: "%" },
    { id: "cab_dist", name: "Distance", min: 0, max: 100, val: 30, unit: "%" },
  ],
  oversized_412: [
    { id: "cab_mic_type", name: "Mic Type", min: 0, max: 2, val: 0, unit: "" },
    { id: "cab_mic", name: "Mic Pos", min: 0, max: 100, val: 32, unit: "%" },
    { id: "cab_dist", name: "Distance", min: 0, max: 100, val: 18, unit: "%" },
  ],
  bass_cabinet: [
    { id: "cab_mic_type", name: "Mic Type", min: 0, max: 2, val: 1, unit: "" },
    { id: "cab_mic", name: "Mic Pos", min: 0, max: 100, val: 60, unit: "%" },
    { id: "cab_dist", name: "Distance", min: 0, max: 100, val: 35, unit: "%" },
  ],
  brit_412: [
    { id: "cab_mic_type", name: "Mic Type", min: 0, max: 2, val: 0, unit: "" },
    { id: "cab_mic", name: "Mic Pos", min: 0, max: 100, val: 38, unit: "%" },
    { id: "cab_dist", name: "Distance", min: 0, max: 100, val: 25, unit: "%" },
  ],
  uber_412: [
    { id: "cab_mic_type", name: "Mic Type", min: 0, max: 2, val: 0, unit: "" },
    { id: "cab_mic", name: "Mic Pos", min: 0, max: 100, val: 48, unit: "%" },
    { id: "cab_dist", name: "Distance", min: 0, max: 100, val: 18, unit: "%" },
  ],
  slo_412: [
    { id: "cab_mic_type", name: "Mic Type", min: 0, max: 2, val: 0, unit: "" },
    { id: "cab_mic", name: "Mic Pos", min: 0, max: 100, val: 42, unit: "%" },
    { id: "cab_dist", name: "Distance", min: 0, max: 100, val: 28, unit: "%" },
  ],
};

export const chainOrder: CategoryId[] = [
  "dyn",
  "comp",
  "wah",
  "dist",
  "amp",
  "eq",
  "mod",
  "delay",
  "verb",
  "cab",
];

/** Number of DSP path slots (mirrors Rust `PATH_SLOTS`). */
export const PATH_SLOTS = 10;

/** Index used by DSP `path_slot_*` / `StageKind`. Append-only — these values
 * are the Rust `StageKind` discriminants (comp/eq appended as 7/8, wah as 9). */
export const stageIndex: Record<CategoryId, number> = {
  dyn: 0,
  dist: 1,
  amp: 2,
  mod: 3,
  delay: 4,
  verb: 5,
  cab: 6,
  comp: 7,
  eq: 8,
  wah: 9,
};

export const stageByIndex: CategoryId[] = [
  "dyn",
  "dist",
  "amp",
  "mod",
  "delay",
  "verb",
  "cab",
  "comp",
  "eq",
  "wah",
];

/** Pack a path into the DSP slots (empty = -1). */
export function pathToSlotValues(path: CategoryId[]): number[] {
  const slots = Array.from({ length: PATH_SLOTS }, () => -1);
  path.forEach((cat, i) => {
    if (i < PATH_SLOTS) slots[i] = stageIndex[cat];
  });
  return slots;
}

/** Factory default path. The wah is never tonally neutral, so it starts in
 * the rack and joins the path only when the user places it. */
export function defaultPath(): CategoryId[] {
  return chainOrder.filter((c) => c !== "wah");
}

export function emptyPath(): CategoryId[] {
  return [];
}

export function rackFromPath(path: CategoryId[]): CategoryId[] {
  const inPath = new Set(path);
  return chainOrder.filter((c) => !inPath.has(c));
}

export const icons: Record<string, string> = {
  gate: '<rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>',
  drive:
    '<polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/>',
  amp: '<rect x="2" y="3" width="20" height="14" rx="2"/><line x1="2" y1="10" x2="22" y2="10"/><circle cx="6" cy="14" r="1"/><circle cx="10" cy="14" r="1"/>',
  comp: '<path d="M3 18c3 0 3-8 6-8s3 4 6 4 3-2 6-2"/><line x1="3" y1="6" x2="21" y2="6"/>',
  eq: '<line x1="6" y1="4" x2="6" y2="20"/><line x1="12" y1="4" x2="12" y2="20"/><line x1="18" y1="4" x2="18" y2="20"/><circle cx="6" cy="14" r="2"/><circle cx="12" cy="8" r="2"/><circle cx="18" cy="16" r="2"/>',
  mod: '<path d="M2 12s2-6 5-6 5 12 10 12 5-6 5-6"/>',
  delay: '<circle cx="12" cy="12" r="9"/><polyline points="12 7 12 12 16 14"/>',
  reverb: '<path d="M12 3v18M17 6v12M22 10v4M7 6v12M2 10v4"/>',
  cab: '<ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M3 5v14c0 1.66 4 3 9 3s9-1.34 9-3V5"/>',
  wah: '<path d="M5 20 L9 4 L15 4 L19 20 Z"/><line x1="7" y1="15" x2="17" y2="15"/>',
};

export function fmt(val: number, unit: string): string {
  if (unit === "" || unit === "s") return `${val.toFixed(1)}${unit}`;
  return `${Math.round(val)}${unit}`;
}

/** Canonical default for a parameter within a given model, if defined. */
export function defaultValueFor(
  modelId: string,
  paramId: string,
): number | undefined {
  return parameterDefaults[modelId]?.find((p) => p.id === paramId)?.val;
}

export function cloneParameters(): Record<string, Param[]> {
  const out: Record<string, Param[]> = {};
  for (const [modelId, params] of Object.entries(parameterDefaults)) {
    out[modelId] = params.map((p) => ({ ...p }));
  }
  return out;
}

/**
 * Build the complete parameter state for a factory preset. Presets contain
 * only their intentional overrides, so never layer one over the currently
 * edited state: that would leak controls from the previously selected preset.
 */
export function parametersForPreset(preset: Preset): Record<string, Param[]> {
  const parameters = cloneParameters();
  const modelParams = parameters[preset.model];
  if (!modelParams) return parameters;

  parameters[preset.model] = modelParams.map((param) => {
    const value = preset.values[param.id];
    return value === undefined ? param : { ...param, val: value };
  });
  return parameters;
}
