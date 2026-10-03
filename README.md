# Overhead

Live aircraft over wherever you are, inside Claude Code. You hear a jet, glance down, and it's already named in your status line: `✈ UAL88 · Boeing 787-9 to Tokyo · 3,200 ft climbing · 2.1 nm NE`, the one you're actually hearing, not just the nearest on a map. When something worth seeing happens (a low pass, a police helicopter, an aircraft you've asked to watch for, an emergency), a radar drops in on it. Ask Claude "what's that plane?" and it answers from live data.

- **Radar**: aircraft around home over a real map (coastline and sea, lakes and rivers, runways, airport codes from OpenStreetMap), colored by altitude on tar1090's scale or by what they're doing, with trails, a one-minute leader line, and labels. Drag to pan, double-click to zoom.
- **Emergencies and interesting aircraft**: an aircraft squawking 7500/7600/7700 or declaring an emergency flashes red, goes to the top of the list and the status line, and raises a toast at any hour. Military aircraft are tagged and toasted; helicopters are tagged.
- **The picked aircraft**: its whole flight so far, colored by altitude, a dashed great-circle line on to its destination, both airports, the distance to go and an ETA. `f` follows it. Links open it in Flightradar24, FlightAware (schedules and gates) or adsb.lol (the same unfiltered data Overhead uses, exact airframe, military included); `o` opens Flightradar24.
- **Window view**: a first-person view of the sky from home. The sun position, cloud cover and skyline come from your real location, and each aircraft is drawn at its true bearing and elevation. It faces the busiest airport nearby unless you point it.
- **Who's flying**: the five nearest aircraft, each on a digit key, with the airline, aircraft model, registration and route (PIT → Boston). Routes are checked against the aircraft's position, so a stale route database can't show a wrong one.
- **Interesting aircraft**: about 17,000 airframes spotters have tagged (plane-alert-db): police, air ambulances, governments, coast guard, firefighters, historic, air forces. A ★ on the radar, a toast, and their own tally on the life list.
- **Watches in plain English**: `/radar watch any 747 within 20 miles` becomes a standing alert (one quick model call to understand it, then checked locally on every poll). Claude can set them too, through `overhead_watch`.
- **The airport strip**: the busiest airport nearby, its live METAR (flight category, wind, ceiling), the runways it's landing and departing on (read from the aircraft lined up with them), and where that traffic passes you: "arrivals for 27 pass 1.8 nm S of you at ~2,600 ft". Landing runways show green on the radar, approach dashed.
- **Photos**: the picked aircraft's photo from Planespotters, credited and linked; pixels in kitty/Ghostty, cells elsewhere, a picture in Desktop.
- **Rewind and the session line**: scrub the radar back up to 30 minutes, and get a line under each longer answer saying what flew over while Claude worked.
- **`overhead_now`**: a tool Claude can call to answer questions about the aircraft around you.
- **Pass alerts**: a toast when an aircraft is about to pass within 1 km, with quiet hours.
- **Life list**: every type and operator you've seen from home, with a toast for new or rare ones.
- **A status line**: the aircraft you're most likely hearing (closest through the air, so a low climber beats a jet at 35,000 ft overhead), kept current while the pane is closed.
- **Scope theme**: `t` turns the radar into an old ATC scope: green phosphor, a sweep line, blips that flare as it passes and fade after.

Home is your city from your IP address by default, looked up again each session so it follows your laptop. Pin it exactly with `/radar home <place>`.

## Install

Requires Claude Code 2.1.287 or later, on macOS or Linux.

```
claude plugin marketplace add trevorstenson/overhead
claude plugin install overhead@overhead
```

Then run `/radar`. The first run downloads the `skyd` helper for your machine (a few MB) from this repository's GitHub releases and checks its SHA-256.

## Terminals

| Terminal | What you see |
| --- | --- |
| Ghostty, kitty | Full-resolution pixels at 30 fps, read straight from shared memory |
| Warp, iTerm2, Terminal.app, WezTerm, VS Code, others | The same frames drawn as colored cells, each a 2×2 block of pixels shown as the quadrant character (▘▝▖▗▚▞▙▛▜▟…) that fits it best, with labels as real text |
| Claude Desktop (Code tab) | A vector radar drawn as SVG: map, altitude colors, emergencies, the picked aircraft's track and route |

Claude Code draws its `Image` element only in kitty and Ghostty; everywhere else Overhead uses a `Raster` of cells. A smaller font size gives the cell version more detail. Over SSH or inside tmux, the cell version is used.

## Commands

| Command | What it does |
| --- | --- |
| `/radar` | Open or close the radar. Opening it yourself keeps it open between turns |
| `/radar view radar\|window` | Top-down, or the sky out of a window (`v` in the pane swaps) |
| `/radar theme map\|scope` | The map, or an old ATC scope (`t`) |
| `/radar fov <deg>` | How wide the window view is (about 60° for a window seen from a desk); `/radar calibrate` walks through lining it up, `j`/`k` nudge 2° |
| `/radar face <dir>` | Which way the window faces: `N`, `SW`, `135`, or `auto` |
| `/radar home [<place>\|auto]` | Show home, pin it to a place (`Somerville, MA`, `Lyon`, `48.85,2.35`), or follow your IP again |
| `/radar range <nm>` | How far the radar reaches (`z`/`x` zoom in the pane) |
| `/radar zoom in\|out` | Step the range from the prompt |
| `/radar center` | Put home back in the middle after dragging (`c`) |
| `/radar follow` | Keep the picked aircraft in the middle (`f`) |
| `/radar open` | Open the picked aircraft in Flightradar24 (`o`) |
| `/radar watch <words>` | A standing alert, e.g. `any police helicopter`, `A380s within 20 km`; `/radar watch` lists them, `/radar unwatch <n\|all>` removes |
| `/radar rewind 10m\|live` | The sky as it was, up to 30 min back (`b` back a minute, `n` forward, `l` live) |
| `/radar photos on\|off` | The picked aircraft's photo (`p`) |
| `/radar summary on\|off` | The line under longer answers about what flew over |
| `/radar colors altitude\|status` | Color by height, or by arriving/departing/cruising (`a`) |
| `/radar auto events\|always\|off` | When the pane drops in by itself: when something worth seeing happens (the default once you've used `/radar`), 5 s into every turn, or never |
| `/radar alerts on\|off\|quiet 22-7` | Pass alerts, and the hours they stay quiet |
| `/radar list` | Your life list |
| `/radar off` | Stop polling and dropping in until the next `/radar` |

In the pane, `1`–`5` pick an aircraft from the list, and clicking one on the picture picks it too. Drag the radar to move around the map, double-click to zoom in on a spot, and press `c` (or `/radar center`) to put home back in the middle. Dragging the window view turns it. Pane keys work once the pane has focus: `/radar` gives it focus, and ctrl+x tab or a click does too.

The pane gets out of the way the moment Claude needs you: a permission prompt or a question closes a pane that opened by itself. One that dropped in for an event leaves after 90 s unless you touch it. One you opened stays.

Pass alerts are for low passes (3,000 ft or below) and novel aircraft only; under a busy approach, routine low passes toast at most once every 15 minutes.

## Data and privacy

Overhead uses free, keyless public services:

- **Positions**: [adsb.lol](https://adsb.lol), polled every 5 s while the pane is open and every 15 s while it's closed.
- **Routes**: adsb.lol's route files, then [adsbdb](https://www.adsbdb.com).
- **Aircraft details**: [adsbdb](https://www.adsbdb.com).
- **Location**: [ipinfo.io](https://ipinfo.io), then [ipwho.is](https://ipwho.is). Your IP address goes to these to find your city. Pin home with `/radar home <place>` to skip them.
- **Weather, terrain, place names**: [Open-Meteo](https://open-meteo.com).
- **Map**: [OpenStreetMap](https://www.openstreetmap.org/copyright) through the Overpass API, once per place.
- **Flight tracks**: adsb.lol's trace files, for the picked aircraft only.
- **Interesting aircraft**: [plane-alert-db](https://github.com/sdr-enthusiasts/plane-alert-db), downloaded weekly.
- **Airport weather**: [aviationweather.gov](https://aviationweather.gov/data/api/) METARs, every 10 minutes.
- **Photos**: [Planespotters.net](https://www.planespotters.net), for the picked aircraft only, credited to the photographer.

If adsb.lol rate-limits your IP, Overhead waits as long as it asks (`Retry-After`), keeps the aircraft where they were, and says so in the pane.

Routes, aircraft details and the skyline are cached under `~/Library/Caches/overhead` (or `~/.cache/overhead`) and shared by every session.

## How it works

The mod (`hooks/register.js`) is thin: it owns the pane, the commands, the tool and the lifecycle. `skyd`, a Rust helper, owns the data and the drawing: polling, dead reckoning between polls, route and aircraft lookups, pass prediction, and rendering. They talk in lines: `skyd` prints `@frame`, `@cells`, `@aircraft`, `@overhead` and `@select`, and the mod rewrites a small control file that `skyd` re-reads every frame. In Ghostty and kitty, frames go from `skyd` to the terminal through POSIX shared memory, so no pixel passes through the mod.

The approach to drawing pixels comes from [intermission](https://github.com/jarrodwatts/intermission), which plays Doom the same way.

## Develop

```
cd skyd && cargo build --release && cargo test --release && cd ..
claude --plugin-dir .            # loads the mod and reloads it on save
claude plugin validate --strict .
claude plugin test .
```

`skyd` can draw a single frame for a quick look without Claude Code:

```
skyd/target/release/skyd run --mode ansi --view window --columns 120 --rows 32 --lat 42.39 --lon -71.10
skyd/target/release/skyd run --mode ppm --view radar --lat 42.39 --lon -71.10 > radar.ppm
```

`--source file:skyd/tests/fixtures/boston.json` replays a recorded response instead of polling. `--at <unix time>` and `--cloud 0..1` set the window view's sky.

Tested with Claude Code 2.1.288.
