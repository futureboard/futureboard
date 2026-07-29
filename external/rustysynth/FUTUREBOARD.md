# Futureboard rustysynth patch

Vendored from crates.io `rustysynth` 1.3.6.

Local changes vs upstream:

1. **`sanitize_regions`** — drop invalid instrument regions instead of failing
   the entire SoundFont load. Many real SF2 banks leave inverted/empty loop
   points on NoLoop regions; rejecting the whole bank caused
   `SanityCheckFailed` for fonts that play fine in FluidSynth / other DAWs.
2. **`read_wave_data`** — decode `smpl` as explicit little-endian i16 so
   big-endian hosts match LE hosts.
