// Overhead: live aircraft over wherever you are, drawn in a pane.
//
// skyd, the Rust helper, owns the data and the drawing; this module owns home,
// the pane, the commands and the status line. kitty and Ghostty get shm Image
// frames (pixels never cross this module); every other terminal gets a Raster
// of cells that skyd encodes and this module relays.
//
// Home comes from the public IP by default, looked up again each session so it
// follows a laptop around, or is pinned with /radar home <place>.
//
// Once someone has used /radar, the pane also drops in while Claude works:
//   idle      nothing showing, whether or not Claude is working
//   waiting   Claude is working; opening once the delay passes
//   offered   the terminal was too narrow for the pane to open by itself, so
//             the band above the prompt offers a key that opens it
//   auto      opened by itself; it goes when the turn ends or Claude needs you
//   pinned    opened by the person; it stays until they close it

const PANE = 'overhead'
const WIDTH = 960
const HEIGHT = 540
const DEFAULT_RANGE_NM = 12
const RANGES_NM = [3, 5, 8, 12, 20, 30, 50]
// How many of the nearest aircraft the pane lists, each on a digit
const LISTED = 5
// An IP location older than this is looked up again before it's used
const IP_HOME_TTL_MS = 6 * 60 * 60 * 1000

// How long Claude works before the pane drops in
const AUTO_DELAY_MS = 5000
// How long overhead_now waits for a first fix when nothing was polling
const TOOL_WAIT_MS = 8000
const TOOL = 'mcp__overhead__overhead_now'
// No pass alerts between these local hours, unless changed
const DEFAULT_QUIET = { from: 22, to: 7 }

// Settings, kept in $.store between sessions
let home = null // { lat, lon, label, source: 'ip' | 'place', at }
let rangeNm = DEFAULT_RANGE_NM
// `radar`, top-down, or `window`, the sky from home facing `face`
let view = 'radar'
// A bearing in degrees, or 'auto' for the busiest airport nearby
let face = 'auto'
// How many degrees of sky the window view spans: matched to a real window,
// which is usually narrower than the 90° default
let fov = 90
// 'altitude', tar1090's colors by height, or 'status': arriving, departing…
let colors = 'altitude'
// Keeping the picked aircraft in the middle of the radar
let isFollowing = false
// How far into the past the radar shows, in seconds (0 is live), and how far
// back skyd can go
let rewindSec = 0
let historySec = 0
// A line under Claude's answer about what flew over while it worked
let isSummary = true
// What's been seen during the turn under way, for that line
let turnSky = null
// The busiest airport nearby, as skyd reads it: runways in use and its METAR
let ops = null
// The picked aircraft's photo as skyd sent it, and whether photos show
let photo = null
let isPhotoShown = true
// How many cells wide the photo is drawn, from the pane's width
let photoColumns = 36
// Standing watches, in plain English and as rules: [{ id, text, rule, created }]
let watches = []
// When each watch last fired for each aircraft, by "id:hex"
const watchFired = new Map()
// When the pane drops in by itself: 'events' (something worth seeing just
// happened), 'always' (five seconds into every turn), or 'off'. Events once
// /radar has been used.
let autoMode = 'off'
// A pane dropped in for an event leaves after this, unless it's touched
const EVENT_STAY_MS = 90 * 1000
let eventTimer = null
// Routine low passes toast at most this often; novel aircraft always do
const ROUTINE_PASS_GAP_MS = 15 * 60 * 1000
const LOW_PASS_FT = 3000
let lastRoutinePass = -Infinity
// Toasts when an aircraft is about to pass overhead
let isAlerting = true
let quiet = DEFAULT_QUIET

let phase = 'idle'
let isTurnRunning = false
// Closed by hand during this turn, so stay out until the next one
let isDismissed = false
let timer = null

// The running helper's output stream, its control file, how it draws, and
// what it drew last
let skyd = null
let skydMode = null
let controlPath = null
let size = null
let frame = null
let cells = null
let aircraft = []
// Whether this skyd has answered at least once, empty sky included
let received = false
// skyd ended without being asked: wait before starting it again, so one that
// can't run isn't restarted on every redraw
let isBackingOff = false
// Passes already announced, by hex, so a restarted skyd can't repeat one
const announced = new Map()

// Every type and operator seen from home: { types: { A333: { name, first, seen } },
// operators: { 'Aer Lingus': { first, seen } } }, kept in $.store
let lifeList = null
// Seen this session, so each aircraft counts once however often it's polled
const counted = new Set()
// The life list's updates, in order
let lifeListWork = Promise.resolve()
// Types worth a toast every time, not just the first
const RARE_TYPES = new Set([
  'A388', 'B748', 'B744', 'B742', 'A124', 'A225', 'A3ST', 'A337', 'BLCF', 'B52', 'C5M', 'C17', 'C130', 'C30J',
  'K35R', 'E3TF', 'E6', 'P8', 'B1', 'B2', 'F35', 'F16', 'F15', 'F18S', 'FA18', 'V22', 'CONC', 'DC3', 'B17', 'P51',
])
let lastError = null
let isOpen = false
// The hex of the aircraft the person picked, highlighted on the radar
let selected = null
// The latest click on the picture, which skyd answers with @select
let click = null
// Where the middle of the radar is, nm east and north of home, once dragged
let pan = { x: 0, y: 0 }
// How many nm the radar picture spans across and down, as skyd last said
let span = null
// The bearing the window faces now, as skyd last said (it picks one on auto)
let faceNow = null
let faceSaveTimer = null
// Drawn in the Desktop app, which takes SVG and no pixels: skyd only polls,
// and this module draws the radar itself
let isDesktop = false
// When the aircraft list arrived, for moving them on between polls
let aircraftAt = 0
// The map and the picked aircraft's track, as skyd sends them for the SVG
// radar: { coast, runways, airports } and [[east, north, alt], …] in nm
let outline = null
let pickedTrace = []
// Frames relayed in the last whole second, for the readout
let framesThisSecond = 0
let fps = 0
let fpsTimer = null

// ---- skyd, built here or downloaded -----------------------------------------

// Where release builds of skyd are published, one per plugin version
const RELEASES = 'https://github.com/trevorstenson/overhead/releases/download'

// The skyd this session runs, once found
let skydBinary = null
// A download in flight, shared by everyone waiting on it
let download = null

// A local `cargo build --release` wins, so development needs no release
async function findSkyd($) {
  for (const path of [$.plugin.root + '/skyd/target/release/skyd', $.plugin.root + '/dist/skyd']) {
    if (await $.fs.exists(path)) return (skydBinary = path)
  }
  return null
}

// skyd, downloading this version's build for this machine the first time;
// false, with the reason in lastError, when there's none to be had
async function ensureSkyd($) {
  if (skydBinary || (await findSkyd($))) return true
  download ??= downloadSkyd($).finally(() => {
    download = null
  })
  await download
  return (await findSkyd($)) !== null
}

async function downloadSkyd($) {
  const root = $.plugin.root
  const say = (text) => {
    lastError = text
    $.ui.invalidate('ui.render')
  }
  try {
    const [system, machine] = (await $.process.run(['uname', '-sm'])).stdout.trim().split(' ')
    const os = { Darwin: 'macos', Linux: 'linux' }[system]
    const arch = { arm64: 'arm64', aarch64: 'arm64', x86_64: 'x86_64' }[machine]
    if (!os || !arch) throw new Error('no build for ' + system + ' ' + machine + '; build it with cargo in ' + root + '/skyd')
    const { version } = JSON.parse(await $.fs.read(root + '/.claude-plugin/plugin.json'))
    const asset = RELEASES + '/v' + version + '/skyd-' + os + '-' + arch + '.tar.gz'
    const archive = root + '/skyd-download.tar.gz'
    say('Downloading skyd for ' + os + ' ' + arch + '…')
    const fetched = await $.process.run(['curl', '-fsSL', '--retry', '2', '-o', archive, asset], { timeoutMs: 5 * 60 * 1000 })
    if (fetched.exitCode !== 0) throw new Error(fetched.stderr.trim() || 'the download failed')
    // The release publishes each archive's SHA-256 beside it
    const expected = (await $.process.run(['curl', '-fsSL', '--retry', '2', asset + '.sha256'])).stdout.trim().split(/\s+/)[0]
    const actual = (await $.process.run(['shasum', '-a', '256', archive])).stdout.trim().split(/\s+/)[0]
    if (!expected || expected !== actual) {
      await $.process.run(['rm', '-f', archive])
      throw new Error("the download didn't match its checksum, so it was thrown away")
    }
    await $.process.run(['mkdir', '-p', root + '/dist'])
    const unpacked = await $.process.run(['tar', '-xzf', archive, '-C', root + '/dist'])
    await $.process.run(['rm', '-f', archive])
    if (unpacked.exitCode !== 0) throw new Error(unpacked.stderr.trim() || 'the download was damaged')
    say(null)
  } catch (error) {
    say("Couldn't get skyd: " + error.message)
  }
}

// kitty and Ghostty read shm images; the rest show an Image's alt text
async function drawMode($) {
  const program = ((await $.env.get('TERM_PROGRAM')) || '').toLowerCase()
  const term = ((await $.env.get('TERM')) || '').toLowerCase()
  if (program === 'ghostty' || term.includes('kitty') || term.includes('ghostty')) return 'image'
  return 'raster'
}

// Cells are about twice as tall as wide, so a 16:9 picture is
// columns * 9/16 / 2 rows. A pane docked beside the transcript also has a
// height to stay within: a Raster just gets shorter (skyd draws to whatever
// shape it's given), an Image keeps 16:9.
function fit(mode, bodyColumns, maxRows) {
  let columns = Math.max(1, Math.min(255, bodyColumns))
  let rows = Math.max(1, Math.round((columns * HEIGHT) / WIDTH / 2))
  if (maxRows && rows > maxRows) {
    rows = Math.max(4, maxRows)
    if (mode === 'image') columns = Math.max(1, Math.min(columns, Math.round((rows * 2 * WIDTH) / HEIGHT)))
  }
  return { columns, rows }
}

// ---- home ----------------------------------------------------------------

// Runs a one-shot skyd subcommand that prints one line of JSON
async function skydJson($, args) {
  if (!(await ensureSkyd($))) throw new Error(lastError ?? 'skyd is missing')
  const { exitCode, stdout } = await $.process.run([skydBinary, ...args], { timeoutMs: 20000 })
  const line = stdout.trim()
  if (exitCode !== 0 || line.startsWith('@error ')) throw new Error(line.replace(/^@error /, '') || 'skyd failed')
  return JSON.parse(line)
}

async function setHome($, place) {
  const moved = !home || distanceNm(home, place) > 1
  home = { ...place, at: await $.clock.now() }
  await $.store.set('home', home)
  // A new centre means a new feed query, so start skyd over
  if (moved && skyd) await restartSkyd($)
}

// Home as stored, looked up from the IP when there is none or it's stale
async function ensureHome($) {
  const now = await $.clock.now()
  if (home && (home.source === 'place' || now - home.at < IP_HOME_TTL_MS)) return home
  try {
    await setHome($, await skydJson($, ['locate']))
  } catch (error) {
    // A stale home beats none
    if (!home) throw error
    $.ui.log('overhead: locate failed, keeping ' + home.label + ': ' + error.message, { to: 'debug' })
  }
  return home
}

function distanceNm(a, b) {
  const dy = (a.lat - b.lat) * 60
  const dx = (a.lon - b.lon) * 60 * Math.cos((a.lat * Math.PI) / 180)
  return Math.hypot(dx, dy)
}

function homeText() {
  if (!home) return 'locating…'
  return home.source === 'ip' ? 'near ' + home.label + ' (from your IP)' : home.label
}

// ---- skyd ----------------------------------------------------------------

async function writeControl($) {
  if (!controlPath) return
  const parts = ['range', String(rangeNm), 'paused', isOpen && !isDesktop ? '0' : '1', 'select', selected ?? '-', 'view', view, 'face', String(face)]
  if (size) parts.push('columns', String(size.columns), 'rows', String(size.rows))
  if (click) parts.push('click', String(click.n), click.x.toFixed(4), click.y.toFixed(4))
  parts.push('pan', pan.x.toFixed(3), pan.y.toFixed(3), 'colors', colors, 'follow', isFollowing && selected ? '1' : '0', 'rewind', String(rewindSec), 'photo', String(photoColumns), 'fov', String(fov))
  await $.fs.write(controlPath, parts.join(' ') + '\n')
}

async function stopSkyd($) {
  const running = skyd
  skyd = null
  skydMode = null
  frame = null
  cells = null
  aircraft = []
  received = false
  $.ui.status(undefined)
  // Leaving the stream's loop is what stops the helper
  if (running) await running.return()
}

async function restartSkyd($, mode) {
  const nextMode = mode ?? skydMode ?? (await drawMode($))
  await stopSkyd($)
  void runSkyd($, nextMode)
}

async function runSkyd($, mode) {
  if (!home) return
  const id = Math.random().toString(36).slice(2, 6)
  controlPath = '/tmp/overhead-' + id + '.ctl'
  skydMode = mode
  await writeControl($)
  const where = ['--lat', String(home.lat), '--lon', String(home.lon), '--range', String(rangeNm), '--input', controlPath]
  const how =
    mode === 'image'
      ? ['--mode', 'shm', '--width', String(WIDTH), '--height', String(HEIGHT), '--prefix', '/ov' + id + '-']
      : ['--mode', 'cells', '--columns', String(size?.columns ?? 80), '--rows', String(size?.rows ?? 22)]
  if (!skydBinary) return
  const stream = $.process.spawn({ argv: [skydBinary, 'run', ...where, ...how] })
  skyd = stream
  let pending = ''
  try {
    for await (const { stream: which, text } of stream) {
      if (which !== 'stdout') continue
      // Pieces arrive as written, not as lines
      const lines = (pending + text).split('\n')
      pending = lines.pop()
      for (const line of lines) handleLine($, line)
    }
  } catch (error) {
    lastError = 'skyd stopped: ' + error
    $.ui.log('overhead: ' + lastError, { to: 'debug' })
  } finally {
    // A newer helper may already have replaced this one; if not, it ended by
    // itself, so start the next only after a pause
    if (skyd === stream) {
      skyd = null
      skydMode = null
      frame = null
      cells = null
      isBackingOff = true
      lastError ??= 'skyd stopped; starting it again shortly'
      $.clock.after(10000, () => {
        isBackingOff = false
        if (isOpen) $.ui.invalidate('ui.render')
      })
      if (isOpen) $.ui.invalidate('ui.render')
    }
  }
}

function handleLine($, line) {
  if (line.startsWith('@cells ')) {
    // Frames already in the pipe at a resize have the old size
    const [, columns, rows] = line.split(' ', 3)
    if (Number(columns) !== size?.columns || Number(rows) !== size?.rows) return
    const isFirst = cells === null
    cells = line.slice(line.lastIndexOf(' ') + 1)
    framesThisSecond += 1
    if (isFirst) $.ui.invalidate('ui.render')
    else $.ui.blit({ requestId: PANE, key: 'view', cells }).catch(() => {})
    return
  }
  const shm = /^@frame (\S+)/.exec(line)
  if (shm) {
    const isFirst = frame === null
    frame = shm[1]
    framesThisSecond += 1
    if (isFirst) $.ui.invalidate('ui.render')
    else $.ui.blit({ requestId: PANE, key: 'view', source: shmSource(frame) }).catch(() => {})
    return
  }
  if (line.startsWith('@aircraft ')) {
    try {
      aircraft = JSON.parse(line.slice(10))
      aircraftAt = Date.now()
      received = true
      lastError = null
    } catch {
      return
    }
    // The picked aircraft flew out of range
    if (selected && !aircraft.some((a) => a.hex === selected)) {
      selected = null
      void writeControl($)
    }
    $.ui.status(statusText())
    noteTurnSky()
    void checkWatches($)
    // Bookkeeping, one batch after another and each with its own snapshot; a
    // failure here mustn't touch the radar
    const flying = airborne()
    lifeListWork = lifeListWork
      .then(() => recordSightings($, flying))
      .catch((error) => $.ui.log('overhead: life list: ' + error, { to: 'debug' }))
    if (isOpen) $.ui.invalidate('ui.render')
    return
  }
  const scale = /^@scale (\S+) (\S+)/.exec(line)
  if (scale) {
    span = { x: Number(scale[1]), y: Number(scale[2]) }
    return
  }
  if (line.startsWith('@photo ')) {
    try {
      const next = JSON.parse(line.slice(7))
      if (next.hex === selected) photo = next
    } catch {}
    if (isOpen) $.ui.invalidate('ui.render')
    return
  }
  if (line.startsWith('@ops ')) {
    try {
      ops = JSON.parse(line.slice(5))
    } catch {}
    if (isOpen) $.ui.invalidate('ui.render')
    return
  }
  const history = /^@history (\d+)/.exec(line)
  if (history) {
    historySec = Number(history[1])
    return
  }
  if (line.startsWith('@outline ')) {
    try {
      outline = JSON.parse(line.slice(9))
    } catch {}
    return
  }
  if (line.startsWith('@trace ')) {
    try {
      pickedTrace = JSON.parse(line.slice(7))
    } catch {}
    return
  }
  const panned = /^@pan (\S+) (\S+)/.exec(line)
  if (panned) {
    // Follow mode moved the map; keep it there when following stops
    if (isFollowing) pan = { x: Number(panned[1]), y: Number(panned[2]) }
    return
  }
  if (line.startsWith('@alert ')) {
    try {
      announceAlert($, JSON.parse(line.slice(7))).catch(() => {})
    } catch {}
    return
  }
  const faced = /^@face (\S+)/.exec(line)
  if (faced) {
    faceNow = Number(faced[1])
    return
  }
  if (line.startsWith('@select ')) {
    const hex = line.slice(8).trim()
    if ((hex === '-' ? null : hex) !== selected) photo = null
    selected = hex === '-' ? null : hex
    void writeControl($)
    if (isOpen) $.ui.invalidate('ui.render')
    return
  }
  if (line.startsWith('@overhead ')) {
    try {
      announcePass($, JSON.parse(line.slice(10))).catch(() => {})
    } catch {}
    return
  }
  if (line.startsWith('@error ')) {
    lastError = line.slice(7)
    $.ui.log('overhead: skyd: ' + lastError, { to: 'debug' })
  }
}

function shmSource(name) {
  return { shm: name, format: 'rgb', width: WIDTH, height: HEIGHT }
}

// ---- words ---------------------------------------------------------------

const COMPASS = ['N', 'NE', 'E', 'SE', 'S', 'SW', 'W', 'NW']

function compass(bearing) {
  return COMPASS[Math.round(bearing / 45) % 8]
}

// Airborne aircraft nearest first, any in an emergency ahead of the rest
function airborne() {
  const up = aircraft.filter((a) => a.status !== 'ground')
  return [...up.filter((a) => a.emergency), ...up.filter((a) => !a.emergency)]
}

// "⚠ 7700" before an aircraft in an emergency, "MIL" or "heli" after a few others
function flags(a) {
  if (a.emergency) return { before: '⚠ ' + (/^7[567]00$/.test(a.squawk ?? '') ? a.squawk : a.emergency.toUpperCase()) + ' ', after: '' }
  return { before: '', after: a.military ? ' · MIL' : a.interest ? ' · ★ ' + a.interest.group : a.rotorcraft ? ' · heli' : '' }
}

function faceText() {
  return face === 'auto' ? 'the busiest airport nearby' : COMPASS[Math.round(face / 45) % 8] + ' (' + face + '°)'
}

// ---- the Desktop app's radar, as SVG ----------------------------------------

const SVG_W = 720
const SVG_H = 420
const STATUS_COLORS = { arrival: '#4ade80', departure: '#fb923c', cruise: '#e5e7eb', ground: '#6b7280' }
const EMERGENCY_COLOR = '#ef4444'
const MILITARY_COLOR = '#c4a5fa'

function escapeXml(text) {
  return String(text).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c])
}

// tar1090's altitude scale, as skyd draws it: orange low, magenta at cruise
const ALTITUDE_STOPS = [[0, 20], [2000, 32.5], [4000, 43], [6000, 54], [8000, 72], [9000, 85], [11000, 140], [40000, 300]]

function altitudeColor(alt) {
  if (alt == null) return '#6b7280'
  const a = Math.min(40000, Math.max(0, alt))
  const i = Math.max(0, ALTITUDE_STOPS.findIndex(([at]) => a <= at) - 1)
  const [[a0, h0], [a1, h1]] = [ALTITUDE_STOPS[i], ALTITUDE_STOPS[i + 1]]
  return 'hsl(' + (h0 + ((a - a0) / (a1 - a0)) * (h1 - h0)).toFixed(0) + ' 85% 55%)'
}

function colorOf(a) {
  if (a.emergency) return EMERGENCY_COLOR
  if (colors === 'altitude' && a.status !== 'ground') return altitudeColor(a.alt_ft)
  return STATUS_COLORS[a.status] ?? '#e5e7eb'
}

// A point given in nm from home, as the latest list placed it, moved on along
// its track for the time since
function positionOf(a, elapsed) {
  const b = (a.bearing_deg * Math.PI) / 180
  let [x, y] = [a.distance_nm * Math.sin(b), a.distance_nm * Math.cos(b)]
  if (a.track_deg != null && a.status !== 'ground') {
    const nm = (a.gs_kt / 3600) * elapsed
    const t = (a.track_deg * Math.PI) / 180
    x += nm * Math.sin(t)
    y += nm * Math.cos(t)
  }
  return [x, y]
}

// nm east and north of home, as skyd projects it
function fromHome(lat, lon) {
  return [(lon - home.lon) * 60 * Math.cos((home.lat * Math.PI) / 180), (lat - home.lat) * 60]
}

// The radar as the terminal draws it: the map, rings, aircraft colored by
// altitude or status, emergencies flashing, and the picked one's track and
// route on. Redrawn every second.
function radarSvg() {
  const [cx, cy] = [SVG_W / 2, SVG_H / 2]
  const scale = (Math.min(cx, cy) / rangeNm) * 0.95
  const elapsed = Math.min(20, Math.max(0, (Date.now() - aircraftAt) / 1000))
  const flash = Date.now() % 1000 < 500
  const px = ([x, y]) => [cx + (x - pan.x) * scale, cy - (y - pan.y) * scale]
  const xy = (p) => p[0].toFixed(1) + ' ' + p[1].toFixed(1)
  const onScreen = ([x, y]) => x > -40 && y > -40 && x < SVG_W + 40 && y < SVG_H + 40
  const parts = [
    '<svg xmlns="http://www.w3.org/2000/svg" width="' + SVG_W + '" height="' + SVG_H + '" viewBox="0 0 ' + SVG_W + ' ' + SVG_H + '" overflow="hidden" font-family="ui-monospace,Menlo,monospace" font-size="11">',
    '<rect width="100%" height="100%" fill="#0d121b"/>',
  ]
  if (outline) {
    // Coastline, a path per line, only the stretches near the picture
    const d = []
    for (const line of outline.coast) {
      let drawing = false
      for (const p of line) {
        const q = px(p)
        if (!onScreen(q)) {
          drawing = false
          continue
        }
        d.push((drawing ? 'L' : 'M') + xy(q))
        drawing = true
      }
    }
    parts.push('<path d="' + d.join('') + '" fill="none" stroke="#2a4b6b" stroke-width="1"/>')
    for (const [x1, y1, x2, y2] of outline.runways) {
      const [a, b] = [px([x1, y1]), px([x2, y2])]
      if (onScreen(a)) parts.push('<path d="M' + xy(a) + 'L' + xy(b) + '" stroke="#5d6b7c" stroke-width="' + Math.max(1.5, scale * 0.025).toFixed(1) + '"/>')
    }
    for (const [code, x, y] of outline.airports) {
      const q = px([x, y])
      if (onScreen(q)) parts.push('<text x="' + (q[0] - 10).toFixed(1) + '" y="' + (q[1] + 16).toFixed(1) + '" fill="#6f8aa6">' + escapeXml(code) + '</text>')
    }
  }
  const homeAt = px([0, 0])
  parts.push('<path d="M0 ' + homeAt[1].toFixed(1) + 'H' + SVG_W + 'M' + homeAt[0].toFixed(1) + ' 0V' + SVG_H + '" stroke="#132230"/>')
  const step = rangeNm <= 6 ? 2 : rangeNm <= 20 ? 5 : rangeNm <= 50 ? 10 : 20
  const reach = Math.max(...[[0, 0], [SVG_W, 0], [0, SVG_H], [SVG_W, SVG_H]].map(([x, y]) => Math.hypot(x - homeAt[0], y - homeAt[1]))) / scale
  for (let ring = step; ring < reach; ring += step) {
    parts.push('<circle cx="' + homeAt[0].toFixed(1) + '" cy="' + homeAt[1].toFixed(1) + '" r="' + (ring * scale).toFixed(1) + '" fill="none" stroke="#1f4a3d"/>')
    parts.push('<text x="' + (homeAt[0] + 3).toFixed(1) + '" y="' + (homeAt[1] - ring * scale - 3).toFixed(1) + '" fill="#3f7a66">' + ring + '</text>')
  }
  parts.push('<text x="' + (cx + 3) + '" y="12" fill="#3f7a66">N</text>')

  // The picked aircraft's flight so far, by altitude, and its route on, dashed
  const picked = aircraft.find((a) => a.hex === selected)
  if (picked) {
    for (let i = 1; i < pickedTrace.length; i++) {
      const [a, b] = [px(pickedTrace[i - 1]), px(pickedTrace[i])]
      if (!onScreen(a) && !onScreen(b)) continue
      parts.push('<path d="M' + xy(a) + 'L' + xy(b) + '" stroke="' + (colors === 'altitude' ? altitudeColor(pickedTrace[i][2]) : '#b8c4d6') + '" stroke-opacity="0.85"/>')
    }
    if (picked.route) {
      const [from, to] = [px(fromHome(picked.route.from.lat, picked.route.from.lon)), px(fromHome(picked.route.to.lat, picked.route.to.lon))]
      parts.push('<path d="M' + xy(px(positionOf(picked, elapsed))) + 'L' + xy(to) + '" stroke="#b8c4d6" stroke-dasharray="6 5" stroke-opacity="0.8"/>')
      for (const [q, code] of [[from, picked.route.from.code], [to, picked.route.to.code]]) {
        parts.push('<circle cx="' + q[0].toFixed(1) + '" cy="' + q[1].toFixed(1) + '" r="5" fill="none" stroke="#b8c4d6"/>')
        parts.push('<text x="' + (q[0] - 10).toFixed(1) + '" y="' + (q[1] + 17).toFixed(1) + '" fill="#0a0f1a" stroke="#b8c4d6" stroke-width="3" paint-order="stroke">' + escapeXml(code) + '</text>')
      }
    }
  }

  // Far first, so the nearest lands on top; an emergency on top of all
  const ordered = [...aircraft].reverse().sort((a, b) => Number(Boolean(a.emergency)) - Number(Boolean(b.emergency)))
  for (const a of ordered) {
    const q = px(positionOf(a, elapsed))
    if (!onScreen(q)) continue
    const color = colorOf(a)
    const at = 'translate(' + xy(q) + ')'
    if (a.status === 'ground') {
      parts.push('<circle transform="' + at + '" r="2" fill="' + color + '"/>')
      continue
    }
    parts.push('<path transform="' + at + ' rotate(' + (a.track_deg ?? 0) + ')" d="M0 -7L4.5 5L0 2.5L-4.5 5Z" fill="' + color + '"/>')
    if (a.emergency) parts.push('<circle transform="' + at + '" r="13" fill="none" stroke="' + EMERGENCY_COLOR + '" stroke-width="2" stroke-opacity="' + (flash ? 1 : 0.35) + '"/>')
    if (a.hex === selected) parts.push('<circle transform="' + at + '" r="11" fill="none" stroke="#facc15" stroke-width="1.5"/>')
    const alt = a.alt_ft == null ? '' : ' ' + String(Math.round(a.alt_ft / 100)).padStart(3, '0')
    const tag = a.emergency ? ' ' + (/^7[567]00$/.test(a.squawk ?? '') ? a.squawk : a.emergency.toUpperCase()) : a.military ? ' MIL' : a.rotorcraft ? ' heli' : ''
    const fill = a.hex === selected ? '#facc15' : a.emergency ? EMERGENCY_COLOR : a.military ? MILITARY_COLOR : color
    parts.push('<text x="' + (q[0] + 9).toFixed(1) + '" y="' + (q[1] + 4).toFixed(1) + '" fill="' + fill + '" fill-opacity="0.9">' + escapeXml((a.callsign ?? a.hex) + alt + tag) + '</text>')
  }
  parts.push('<circle cx="' + homeAt[0].toFixed(1) + '" cy="' + homeAt[1].toFixed(1) + '" r="3" fill="#4fd1ff"/>')
  if (colors === 'altitude') {
    // The altitude scale, bottom left
    const stops = [0, 2000, 5000, 10000, 20000, 30000, 40000]
    stops.forEach((alt, i) => {
      parts.push('<rect x="' + (10 + i * 22) + '" y="' + (SVG_H - 18) + '" width="22" height="5" fill="' + altitudeColor(alt) + '"/>')
      parts.push('<text x="' + (10 + i * 22) + '" y="' + (SVG_H - 22) + '" fill="#9aa3b2" font-size="9">' + (alt === 0 ? '0' : alt / 1000 + (alt === 40000 ? 'k' : '')) + '</text>')
    })
  }
  parts.push('</svg>')
  return parts.join('')
}

// ---- links out ---------------------------------------------------------------

// Flightradar24 by flight number, or by registration for a private flight;
// FlightAware by flight number; adsb.lol, unfiltered, by the exact airframe
function links(a) {
  const flight = a.callsign && /^[A-Z]{3}\d/.test(a.callsign) ? a.callsign : null
  return [
    {
      key: 'fr24',
      label: 'Flightradar24',
      href: flight
        ? 'https://www.flightradar24.com/' + flight
        : a.registration
          ? 'https://www.flightradar24.com/data/aircraft/' + a.registration.toLowerCase()
          : 'https://www.flightradar24.com/' + (a.callsign ?? ''),
    },
    ...(flight ? [{ key: 'flightaware', label: 'FlightAware', href: 'https://www.flightaware.com/live/flight/' + flight }] : []),
    { key: 'adsblol', label: 'adsb.lol', href: 'https://adsb.lol/?icao=' + a.hex },
  ]
}

async function openLink($, href) {
  const opener = (await $.process.run(['uname', '-s'])).stdout.trim() === 'Darwin' ? 'open' : 'xdg-open'
  await $.process.run([opener, href])
}

function closestAirborne() {
  return airborne()[0] ?? null
}

// Feet in a nautical mile: altitude and distance on one scale
const FT_PER_NM = 6076

// The aircraft you're most likely hearing: the shortest way through the air,
// not across the ground. A climber at 3,000 ft two miles off beats a jet at
// 35,000 ft straight up.
function heard() {
  let best = null
  let bestNm = Infinity
  for (const a of airborne()) {
    if (a.alt_ft == null) continue
    const nm = Math.hypot(a.distance_nm, a.alt_ft / FT_PER_NM)
    if (nm < bestNm) [best, bestNm] = [a, nm]
  }
  return best
}

// "UA88 · Boeing 787-9 to Tokyo · 3,200 ft climbing · 2.1 nm NE"
function heardText(a) {
  const model = a.model && a.model.length <= 22 ? a.model : a.type
  const to = a.route ? 'to ' + (a.route.to.city || a.route.to.code) : null
  const trend = Math.abs(a.vrate_fpm) < 300 ? '' : a.vrate_fpm > 0 ? ' climbing' : ' descending'
  const { before, after } = flags(a)
  return (
    before +
    [a.callsign ?? a.registration ?? a.hex, [model, to].filter(Boolean).join(' '), a.alt_ft.toLocaleString('en-US') + ' ft' + trend, a.distance_nm + ' nm ' + compass(a.bearing_deg)]
      .filter(Boolean)
      .join(' · ') +
    after
  )
}

function altitude(a) {
  return a.alt_ft == null ? 'ground' : a.alt_ft.toLocaleString('en-US') + ' ft'
}

function routeCodes(a) {
  return a.route ? a.route.from.code + '→' + a.route.to.code : null
}

function describe(a) {
  const name = a.callsign ?? a.registration ?? a.hex
  const { before, after } = flags(a)
  return before + [name, a.type, routeCodes(a), a.distance_nm + ' nm ' + compass(a.bearing_deg), altitude(a)].filter(Boolean).join(' · ') + after
}

// Everything known about one aircraft, for the line under the list
function detail(a) {
  const climb =
    Math.abs(a.vrate_fpm) < 300 ? 'level' : (a.vrate_fpm > 0 ? '↑ ' : '↓ ') + Math.abs(a.vrate_fpm).toLocaleString('en-US') + ' fpm'
  const route = a.route ? (a.route.from.city || a.route.from.code) + ' → ' + (a.route.to.city || a.route.to.code) : null
  // Distance to go and the time it'll take, along the great circle
  const togo =
    a.remaining_nm == null
      ? null
      : Math.round(a.remaining_nm).toLocaleString('en-US') + ' nm to go' + (a.eta_min == null ? '' : ', ' + duration(a.eta_min))
  const { before, after } = flags(a)
  return before + [
    a.callsign ?? a.hex,
    a.owner ?? a.route?.airline,
    [a.model ?? a.type, a.registration].filter(Boolean).join(' ') || null,
    route,
    togo,
    altitude(a) + (a.alt_ft == null ? '' : ' ' + climb),
    Math.round(a.gs_kt) + ' kt',
    a.distance_nm + ' nm ' + compass(a.bearing_deg),
  ]
    .filter(Boolean)
    .join(' · ') + (a.emergency ? ' · ' + a.emergency : '') + after + interestText(a.interest)
}

// What spotters say about it: "★ Police Forces: Massachusetts State Police, Patrol"
function interestText(interest) {
  if (!interest) return ''
  const about = [interest.operator, interest.note].filter(Boolean).join(', ')
  return ' · ★ ' + interest.category + (about ? ': ' + about : '')
}

function duration(minutes) {
  if (minutes < 60) return '~' + Math.max(1, Math.round(minutes)) + ' min'
  return '~' + Math.floor(minutes / 60) + ' h ' + String(Math.round(minutes % 60)).padStart(2, '0') + ' min'
}

// The nearest airborne aircraft, or one in an emergency, which comes first
// An emergency if there is one, else the aircraft you're probably hearing
function statusText() {
  const emergency = airborne().find((a) => a.emergency)
  if (emergency) return describe(emergency)
  const a = heard()
  return a ? '✈ ' + heardText(a) : undefined
}

// ---- pane ----------------------------------------------------------------

// ---- alerts ----------------------------------------------------------------

function isQuietAt(hour) {
  if (quiet.from === quiet.to) return false
  return quiet.from < quiet.to ? hour >= quiet.from && hour < quiet.to : hour >= quiet.from || hour < quiet.to
}

// Under a busy approach a pass is routine many times an hour, so only a low
// one or a novel one (tagged, military, rare, or a type not on your list yet)
// is worth a word, and a routine low one only every quarter hour
async function announcePass($, pass) {
  const now = await $.clock.now()
  if (now - (announced.get(pass.hex) ?? -Infinity) < 10 * 60 * 1000) return
  const info = aircraft.find((a) => a.hex === pass.hex)
  lifeList ??= (await $.store.get('lifeList')) ?? { types: {}, operators: {} }
  const isNovel = Boolean(info?.interest || info?.military || RARE_TYPES.has(pass.type) || (pass.type && !lifeList.types?.[pass.type]))
  const isLow = pass.alt_ft != null && pass.alt_ft <= LOW_PASS_FT
  if (!isNovel && !isLow) return
  if (!isNovel && now - lastRoutinePass < ROUTINE_PASS_GAP_MS) return
  announced.set(pass.hex, now)
  if (!isNovel) lastRoutinePass = now
  if (isQuietAt(new Date(now).getHours())) return
  await dropInFor($, pass.hex)
  if (!isAlerting) return
  const name = pass.callsign ?? pass.hex
  const what = pass.model ?? pass.type
  const route = pass.route ? pass.route.from.code + '→' + pass.route.to.code : null
  const alt = pass.alt_ft == null ? null : 'at ' + pass.alt_ft.toLocaleString('en-US') + ' ft'
  const when = pass.in_s < 5 ? 'overhead now' : 'overhead in ' + pass.in_s + ' s'
  $.ui.toast('✈ ' + [name, what, route].filter(Boolean).join(' · ') + ' ' + [when, alt].filter(Boolean).join(' '), { timeoutMs: 8000 })
}

// An emergency or a military aircraft skyd has spotted, once each. An
// emergency toasts at any hour, while alerts are on; military keeps quiet hours.
async function announceAlert($, alert) {
  const now = await $.clock.now()
  // Emergencies drop in at any hour; the rest keep quiet hours
  if (alert.kind === 'emergency' || !isQuietAt(new Date(now).getHours())) await dropInFor($, alert.hex)
  if (turnSky) (alert.kind === 'emergency' ? turnSky.emergencies : turnSky.interesting).push(alert)
  // On the life list whatever the hour, counted apart from the types
  if (alert.kind !== 'emergency') {
    lifeListWork = lifeListWork.then(() => recordInteresting($, alert, now)).catch(() => {})
  }
  if (!isAlerting) return
  if (alert.kind !== 'emergency' && isQuietAt(new Date(now).getHours())) return
  const name = alert.callsign ?? alert.hex
  const what = alert.model ?? alert.type
  const where = alert.distance_nm + ' nm ' + compass(alert.bearing_deg) + (alert.alt_ft == null ? '' : ' at ' + alert.alt_ft.toLocaleString('en-US') + ' ft')
  if (alert.kind === 'emergency') {
    const code = /^7[567]00$/.test(alert.squawk ?? '') ? 'squawking ' + alert.squawk + ' (' + alert.what + ')' : 'declaring ' + alert.what
    $.ui.toast('⚠ ' + [name, what].filter(Boolean).join(' · ') + ' ' + code + ' · ' + where, { timeoutMs: 15000 })
  } else {
    const label = alert.kind === 'military' ? 'Military' : alert.interest ? capitalize(alert.interest.group) : 'Interesting'
    const who = alert.interest?.operator ? ' (' + alert.interest.operator + ')' : ''
    $.ui.toast('★ ' + label + ': ' + [name, what].filter(Boolean).join(' · ') + who + ' · ' + where, { timeoutMs: 8000 })
  }
}

function capitalize(text) {
  return text.charAt(0).toUpperCase() + text.slice(1)
}

async function recordInteresting($, alert, now) {
  lifeList ??= (await $.store.get('lifeList')) ?? { types: {}, operators: {} }
  lifeList.interesting ??= {}
  if (lifeList.interesting[alert.hex]) return
  lifeList.interesting[alert.hex] = {
    callsign: alert.callsign ?? null,
    group: alert.interest?.group ?? alert.kind,
    category: alert.interest?.category ?? null,
    operator: alert.interest?.operator ?? null,
    first: now,
  }
  await $.store.set('lifeList', lifeList)
}

// ---- standing watches ----------------------------------------------------------

// What a watch can ask about, for Claude and the model alike. Every field
// given must hold; a list holds when any of its values does.
const RULE_FIELDS = {
  label: { type: 'string', description: 'A few words naming the watch, such as "A380s within 20 nm"' },
  types: { type: 'array', items: { type: 'string' }, description: 'ICAO type designators, such as A388, B748, C17' },
  model_contains: { type: 'array', items: { type: 'string' }, description: 'Text in the model name, such as "747" or "Cessna"' },
  operator_contains: { type: 'array', items: { type: 'string' }, description: 'Text in the airline or operator, such as "Lufthansa"' },
  callsign_prefix: { type: 'array', items: { type: 'string' }, description: 'Callsign starts, such as "DAL" or "N"' },
  groups: {
    type: 'array',
    items: { type: 'string', enum: ['military', 'police', 'air ambulance', 'government', 'coast guard', 'firefighting', 'historic', 'notable'] },
    description: "Spotters' tags",
  },
  emergency: { type: 'boolean', description: 'Squawking 7500/7600/7700 or declaring an emergency' },
  rotorcraft: { type: 'boolean', description: 'Helicopters' },
  max_distance_nm: { type: 'number', description: 'Within this many nautical miles of home' },
  min_alt_ft: { type: 'number' },
  max_alt_ft: { type: 'number' },
  status: { type: 'array', items: { type: 'string', enum: ['arrival', 'departure', 'cruise'] } },
  origin: { type: 'array', items: { type: 'string' }, description: 'IATA codes the flight comes from, such as LHR' },
  destination: { type: 'array', items: { type: 'string' }, description: 'IATA codes the flight is going to' },
}

// The fields of a rule that are there and the right kind; null when nothing is
function cleanRule(raw) {
  if (!raw || typeof raw !== 'object') return null
  const rule = {}
  for (const [key, spec] of Object.entries(RULE_FIELDS)) {
    const v = raw[key]
    if (v == null) continue
    if (spec.type === 'array' && Array.isArray(v) && v.length) rule[key] = v.map(String).filter(Boolean)
    else if (spec.type === 'number' && Number.isFinite(Number(v))) rule[key] = Number(v)
    else if (spec.type === 'boolean' && typeof v === 'boolean') rule[key] = v
    else if (spec.type === 'string' && typeof v === 'string' && v.trim()) rule[key] = v.trim()
  }
  return Object.keys(rule).some((k) => k !== 'label') ? rule : null
}

function matchesRule(rule, a) {
  const has = (text, needles) => text != null && needles.some((n) => String(text).toLowerCase().includes(n.toLowerCase()))
  if (rule.types && !rule.types.some((t) => t.toUpperCase() === (a.type ?? '').toUpperCase())) return false
  if (rule.model_contains && !has(a.model, rule.model_contains) && !has(a.type, rule.model_contains)) return false
  if (rule.operator_contains && !has(a.owner, rule.operator_contains) && !has(a.route?.airline, rule.operator_contains) && !has(a.interest?.operator, rule.operator_contains))
    return false
  if (rule.callsign_prefix && !rule.callsign_prefix.some((p) => (a.callsign ?? '').toUpperCase().startsWith(p.toUpperCase()))) return false
  if (rule.groups && !rule.groups.some((g) => a.interest?.group === g || (g === 'military' && a.military))) return false
  if (rule.emergency != null && Boolean(a.emergency) !== rule.emergency) return false
  if (rule.rotorcraft != null && Boolean(a.rotorcraft) !== rule.rotorcraft) return false
  if (rule.max_distance_nm != null && !(a.distance_nm <= rule.max_distance_nm)) return false
  if (rule.min_alt_ft != null && !(a.alt_ft != null && a.alt_ft >= rule.min_alt_ft)) return false
  if (rule.max_alt_ft != null && !(a.alt_ft != null && a.alt_ft <= rule.max_alt_ft)) return false
  if (rule.status && !rule.status.includes(a.status)) return false
  if (rule.origin && !rule.origin.some((c) => c.toUpperCase() === a.route?.from.code)) return false
  if (rule.destination && !rule.destination.some((c) => c.toUpperCase() === a.route?.to.code)) return false
  return true
}

// A rule in words, to read back: "type A388 · within 20 nm"
function ruleText(rule) {
  const out = []
  const list = (v) => v.join('/')
  if (rule.types) out.push('type ' + list(rule.types))
  if (rule.model_contains) out.push('model ' + list(rule.model_contains))
  if (rule.operator_contains) out.push('operator ' + list(rule.operator_contains))
  if (rule.callsign_prefix) out.push('callsign ' + list(rule.callsign_prefix) + '…')
  if (rule.groups) out.push(list(rule.groups))
  if (rule.emergency) out.push('emergencies')
  if (rule.rotorcraft) out.push('helicopters')
  if (rule.status) out.push(list(rule.status))
  if (rule.origin) out.push('from ' + list(rule.origin))
  if (rule.destination) out.push('to ' + list(rule.destination))
  if (rule.max_distance_nm != null) out.push('within ' + rule.max_distance_nm + ' nm')
  if (rule.min_alt_ft != null) out.push('above ' + rule.min_alt_ft.toLocaleString('en-US') + ' ft')
  if (rule.max_alt_ft != null) out.push('below ' + rule.max_alt_ft.toLocaleString('en-US') + ' ft')
  return out.join(' · ')
}

const WATCH_SYSTEM = [
  'You turn a request about aircraft into one JSON object, a rule, and reply with that JSON alone.',
  'Fields (all optional; every field given must hold; a list holds when any of its values does):',
  JSON.stringify(RULE_FIELDS),
  'Use ICAO type designators in "types" (A380 → A388, 747-8 → B748, 747-400 → B744, 787-9 → B789, C-17 → C17).',
  'Prefer "model_contains" for a family ("any 747" → ["747"]). Distances are nautical miles (1 km ≈ 0.54 nm, 1 mi ≈ 0.87 nm).',
  '"Flies over", "overhead" or "over my house" means "max_distance_nm": 1.',
  'Always give "label": a few words naming the watch.',
  'If the request is not about watching for aircraft, reply {"error": "<why>"}.',
].join('\n')

async function addWatch($, text, rule) {
  watches = [...watches, { id: Math.random().toString(36).slice(2, 8), text, rule, created: await $.clock.now() }]
  await $.store.set('watches', watches)
  // Something already in range may match
  await checkWatches($)
}

// Turns a sentence into a rule with a quick model call, once
async function watchFromWords($, words) {
  const reply = await $.model.complete({ model: 'haiku', system: WATCH_SYSTEM, prompt: words, maxTokens: 800 })
  if (!reply.isAnswered) throw new Error("couldn't reach the model (" + (reply.reason ?? 'no reply') + ')')
  const json = /\{[\s\S]*\}/.exec(reply.text)?.[0]
  let raw = null
  try {
    raw = JSON.parse(json ?? '')
  } catch {}
  if (raw?.error) throw new Error(raw.error)
  const rule = cleanRule(raw)
  if (!rule) throw new Error("couldn't make a watch of that")
  return rule
}

// Every watch against what's in the sky: a toast per match, once per aircraft
// every six hours
async function checkWatches($) {
  if (!watches.length) return
  const now = await $.clock.now()
  for (const w of watches) {
    for (const a of aircraft) {
      if (!matchesRule(w.rule, a)) continue
      const key = w.id + ':' + a.hex
      if (now - (watchFired.get(key) ?? -Infinity) < 6 * 3600 * 1000) continue
      watchFired.set(key, now)
      if (!isQuietAt(new Date(now).getHours())) await dropInFor($, a.hex)
      turnSky?.interesting.push({ ...a, kind: 'watch' })
      if (!isAlerting) continue
      const where = a.distance_nm + ' nm ' + compass(a.bearing_deg) + (a.alt_ft == null ? ' on the ground' : ' at ' + a.alt_ft.toLocaleString('en-US') + ' ft')
      $.ui.toast('👁 ' + (w.rule.label ?? w.text) + ': ' + [a.callsign ?? a.hex, a.model ?? a.type, routeCodes(a)].filter(Boolean).join(' · ') + ' · ' + where, { timeoutMs: 10000 })
    }
  }
}

function watchList() {
  if (!watches.length) return 'No watches. /radar watch <what to look for>, such as /radar watch any 747 within 20 miles'
  return watches.map((w, i) => i + 1 + '. ' + (w.rule.label ?? w.text) + ' (' + ruleText(w.rule) + ')').join('\n') + '\n/radar unwatch <number|all> removes one.'
}

// ---- the airport strip -------------------------------------------------------

const CATEGORY_COLORS = { VFR: 'green', MVFR: 'blue', IFR: 'red', LIFR: 'magenta' }

// "BOS · VFR · wind 360° 9 kt · ceiling 3,800 ft · landing 4R 4L · departing 9"
function opsParts() {
  if (!ops) return null
  const m = ops.metar
  const wind = !m || m.wind_kt == null ? null : m.wind_kt === 0 ? 'calm' : 'wind ' + (m.wind_dir == null ? 'variable' : String(m.wind_dir).padStart(3, '0') + '°') + ' ' + m.wind_kt + (m.gust_kt ? 'G' + m.gust_kt : '') + ' kt'
  return {
    airport: ops.airport,
    category: m?.category ?? null,
    rest: [
      wind,
      m?.ceiling_ft != null ? 'ceiling ' + m.ceiling_ft.toLocaleString('en-US') + ' ft' : m ? 'no ceiling' : null,
      m?.visibility && m.visibility !== '10+' ? 'visibility ' + m.visibility + ' sm' : null,
      ops.landing.length ? 'landing ' + ops.landing.slice(0, 3).join(' ') : null,
      ops.departing.length ? 'departing ' + ops.departing.slice(0, 3).join(' ') : null,
      // The prediction that matters at home: where that traffic passes you
      ...(ops.over_home ?? [])
        .slice(0, 1)
        .map((p) => p.kind + ' for ' + p.runway + ' pass ' + p.offset_nm + ' nm ' + p.toward + ' of you at ~' + p.alt_ft.toLocaleString('en-US') + ' ft'),
    ].filter(Boolean),
  }
}

function opsText() {
  const o = opsParts()
  if (!o) return null
  return [o.airport, o.category, ...o.rest].filter(Boolean).join(' · ')
}

// ---- what flew over while Claude worked ------------------------------------

function noteTurnSky() {
  if (!turnSky) return
  for (const a of airborne()) {
    turnSky.seen.add(a.hex)
    if (a.alt_ft != null && (!turnSky.closest || a.distance_nm < turnSky.closest.distance_nm)) turnSky.closest = a
  }
}

// "✈ While Claude worked (4 min): 23 aircraft · closest DAL977 A21N, 0.8 nm
// at 1,500 ft · new for your list: Boeing 747-47UF · ★ MSP1 (police)"
function turnSkyText(minutes) {
  const t = turnSky
  const parts = [t.seen.size + ' aircraft']
  if (t.closest) {
    const c = t.closest
    parts.push('closest ' + [c.callsign ?? c.hex, c.type].filter(Boolean).join(' ') + ', ' + c.distance_nm + ' nm at ' + c.alt_ft.toLocaleString('en-US') + ' ft')
  }
  if (t.newTypes.length) parts.push('new for your list: ' + [...new Set(t.newTypes)].slice(0, 3).join(', '))
  for (const a of t.interesting.slice(0, 2)) parts.push('★ ' + (a.callsign ?? a.hex) + ' (' + (a.interest?.group ?? a.kind) + ')')
  for (const a of t.emergencies.slice(0, 2)) parts.push('⚠ ' + (a.callsign ?? a.hex) + ' ' + (a.squawk ?? a.what))
  return '✈ While Claude worked (' + Math.max(1, Math.round(minutes)) + ' min): ' + parts.join(' · ')
}

// ---- life list ---------------------------------------------------------------

// Adds what's flying now to the life list: a toast for a type never seen
// before, or a rare one, except on the very first look, which only fills it
async function recordSightings($, flying) {
  lifeList ??= (await $.store.get('lifeList')) ?? { types: {}, operators: {} }
  const isFirstLook = Object.keys(lifeList.types).length === 0
  const now = await $.clock.now()
  const news = []
  let changed = false
  for (const a of flying) {
    if (!a.type || counted.has(a.hex)) continue
    counted.add(a.hex)
    changed = true
    const type = (lifeList.types[a.type] ??= { name: a.model ?? null, first: now, seen: 0 })
    type.seen += 1
    type.name ??= a.model ?? null
    if (type.seen === 1 && !isFirstLook) {
      news.push({ a, why: 'new' })
      turnSky?.newTypes.push(a.model ?? a.type)
    }
    else if (RARE_TYPES.has(a.type) && type.seen > 1) news.push({ a, why: 'rare' })
    if (a.owner) {
      const operator = (lifeList.operators[a.owner] ??= { first: now, seen: 0 })
      operator.seen += 1
    }
  }
  if (!changed) return
  await $.store.set('lifeList', lifeList)
  if (isQuietAt(new Date(now).getHours())) return
  // One toast at a time; the list keeps the rest
  const pick = news.find((n) => n.why === 'rare') ?? news[0]
  if (pick) await dropInFor($, pick.a.hex)
  if (!isAlerting) return
  if (pick) {
    const what = (pick.a.model ?? pick.a.type) + (pick.a.owner ? ' · ' + pick.a.owner : '')
    $.ui.toast((pick.why === 'rare' ? '✦ Rare: ' : '✦ New for your list: ') + what + ' (' + (pick.a.callsign ?? pick.a.hex) + ')', { timeoutMs: 8000 })
  }
}

function lifeListText() {
  const types = Object.entries(lifeList?.types ?? {})
  if (!types.length && !Object.keys(lifeList?.interesting ?? {}).length) return 'Nothing on your life list yet: open /radar and let it watch for a while.'
  const operators = Object.keys(lifeList.operators ?? {}).length
  const byFirst = [...types].sort((a, b) => b[1].first - a[1].first).slice(0, 10)
  const byCount = [...types].sort((a, b) => b[1].seen - a[1].seen).slice(0, 5)
  const line = ([code, t]) => '  ' + code.padEnd(5) + (t.name ?? '') + ' · seen ' + t.seen + (RARE_TYPES.has(code) ? ' · rare' : '')
  const interesting = Object.values(lifeList.interesting ?? {})
  const groups = {}
  for (const i of interesting) groups[i.group] = (groups[i.group] ?? 0) + 1
  const special = Object.entries(groups)
    .sort((a, b) => b[1] - a[1])
    .map(([group, n]) => n + ' ' + group)
    .join(', ')
  return [
    types.length + ' types and ' + operators + ' operators seen from home.',
    ...(interesting.length ? ['★ ' + interesting.length + ' interesting aircraft: ' + special + '.'] : []),
    'Newest:',
    ...byFirst.map(line),
    'Most seen:',
    ...byCount.map(line),
  ].join('\n')
}

// ---- the tool Claude calls --------------------------------------------------

const TOOL_DESCRIPTION = [
  "Live aircraft near the user's location right now, from public ADS-B data:",
  'callsign, airline or operator, aircraft model, route, altitude, climb or',
  'descent, speed, and distance and compass direction from the user.',
  'Use it when the user asks what plane is overhead, what they can see or',
  'hear, or anything about the aircraft around them. Location is the',
  "user's approximate city from their IP unless they pinned one.",
].join(' ')

// The aircraft as Claude reads them: one line each, nearest first
function toolText(limit) {
  const where = home.source === 'ip' ? 'near ' + home.label + ' (approximate, from IP)' : home.label
  const up = airborne()
  const lines = [
    'Home: ' + where + ' at ' + home.lat.toFixed(3) + ', ' + home.lon.toFixed(3) + '.',
    up.length + ' airborne and ' + (aircraft.length - up.length) + ' on the ground within about ' + Math.round(rangeNm * 1.5) + ' nm. Directions are from home.',
  ]
  const loud = heard()
  if (loud) lines.push('Most likely the one the user hears (closest through the air): ' + heardText(loud) + '.')
  if (ops) lines.push('Nearest busy airport: ' + opsText() + (ops.metar?.raw ? ' (' + ops.metar.raw + ')' : '') + '.')
  const overhead = up.filter((a) => a.distance_nm <= 1)
  if (overhead.length) lines.push('Directly overhead (within 1 nm): ' + overhead.map((a) => a.callsign ?? a.hex).join(', ') + '.')
  up.slice(0, limit).forEach((a, i) => lines.push(i + 1 + '. ' + detail(a) + ' · ' + links(a)[0].href))
  return lines.join('\n')
}

async function answerTool($, input) {
  try {
    await ensureHome($)
  } catch (error) {
    return "Couldn't tell where the user is: " + error.message + '. They can set it with /radar home <place>.'
  }
  if (!(await ensureSkyd($))) return 'The overhead helper (skyd) is missing: ' + (lastError ?? 'unknown reason')
  // Nothing polling yet: start it paused, as the status line runs, and wait
  // for the first answer
  if (!skyd && !isBackingOff) void runSkyd($, await drawMode($))
  const started = await $.clock.now()
  while (!received && (await $.clock.now()) - started < TOOL_WAIT_MS) await $.clock.sleep(250)
  if (!received) return 'No aircraft data yet' + (lastError ? ': ' + lastError : '') + '. Try again in a few seconds.'
  const limit = Math.max(1, Math.min(25, Number(input?.limit) || 8))
  return toolText(limit)
}

// ---- dropping in for an event ------------------------------------------------

function autoModeText() {
  return autoMode === 'events' ? 'when something worth seeing happens' : autoMode === 'always' ? AUTO_DELAY_MS / 1000 + ' s into every turn' : 'never: only when you ask'
}

// Something worth seeing just happened: open the pane on that aircraft,
// without the keys, and leave after a while unless it's touched
async function dropInFor($, hex) {
  if (autoMode !== 'events' || phase !== 'idle' || isDismissed) return
  const surfaces = await $.session.surfaces()
  if (!(surfaces.includes('terminal') || surfaces.includes('desktop'))) return
  const opened = await $.ui.open({ id: PANE, title: 'overhead' })
  if (!opened.isPlaced) {
    await $.ui.close({ id: PANE })
    return
  }
  phase = 'auto'
  selected = hex
  photo = null
  await showRadar($)
  eventTimer?.cancel()
  eventTimer = $.clock.after(EVENT_STAY_MS, () => {
    eventTimer = null
    if (phase === 'auto') void $.ui.close({ id: PANE })
  })
}

// The person did something in a pane that dropped in: it's theirs now
function keepOpen() {
  if (phase !== 'auto') return
  phase = 'pinned'
  eventTimer?.cancel()
  eventTimer = null
}

// ---- dropping in while Claude works ----------------------------------------

function cancelTimer() {
  timer?.cancel()
  timer = null
}

function armDropIn($) {
  if (autoMode !== 'always' || !isTurnRunning || isDismissed || phase !== 'idle') return
  phase = 'waiting'
  timer = $.clock.after(AUTO_DELAY_MS, () => dropIn($))
}

async function dropIn($) {
  if (phase !== 'waiting') return
  timer = null
  const surfaces = await $.session.surfaces()
  if (!(surfaces.includes('terminal') || surfaces.includes('desktop')) || !(await findSkyd($))) {
    phase = 'idle'
    return
  }
  // Without the focus, so whatever the person is typing stays theirs
  const opened = await $.ui.open({ id: PANE, title: 'overhead' })
  if (!opened.isPlaced) {
    // A pane waiting for room would pop up long after the moment passed; one
    // the person opens appears at any width, so offer them a key instead
    $.ui.log('overhead: the pane waits for a wider terminal: ' + opened.reason, { to: 'debug' })
    await $.ui.close({ id: PANE })
    phase = 'offered'
    $.ui.invalidate('ui.render')
    return
  }
  phase = 'auto'
  await showRadar($)
}

async function acceptOffer($) {
  if (phase !== 'offered') return
  const { isPlaced } = await $.ui.open({ id: PANE, title: 'overhead' })
  if (!isPlaced) return
  phase = 'auto'
  await showRadar($)
}

function withdrawOffer($) {
  cancelTimer()
  phase = 'idle'
  $.ui.invalidate('ui.render')
}

// Claude is about to ask the person something: a pane that dropped in by
// itself gets out of the way; one they opened themselves stays
async function getOutOfTheWay($) {
  if (phase === 'waiting' || phase === 'offered') withdrawOffer($)
  else if (phase === 'auto') await $.ui.close({ id: PANE })
}

// ---- pane ------------------------------------------------------------------

// The person asked for the radar: it stays open between turns
async function openRadar($) {
  cancelTimer()
  // Using the radar once is what lets it drop in later, for events
  if ((await $.store.get('autoMode')) == null) {
    autoMode = 'events'
    await $.store.set('autoMode', autoMode)
  }
  phase = 'pinned'
  // The person asked for it, so its keys are theirs at once; Esc hands them
  // back to the prompt (and only if the composer is empty is focus granted)
  await $.ui.open({ id: PANE, title: 'overhead', focus: true })
  return showRadar($)
}

async function showRadar($) {
  isOpen = true
  framesThisSecond = 0
  fps = 0
  fpsTimer?.cancel()
  fpsTimer = $.clock.every(1000, () => {
    fps = framesThisSecond
    framesThisSecond = 0
    $.ui.invalidate('ui.render')
  })
  if (!(await ensureSkyd($))) return {}
  try {
    await ensureHome($)
  } catch (error) {
    lastError = "couldn't find where you are: " + error.message + '. Set it with /radar home <place>.'
    $.ui.invalidate('ui.render')
    return {}
  }
  // Already running paused for the status line: wake it
  if (skyd) await writeControl($)
  $.ui.invalidate('ui.render')
  return {}
}

// Rewound as far as skyd keeps, and in whole steps
async function setRewind($, seconds) {
  rewindSec = Math.max(0, Math.min(seconds, historySec, 1800))
  await writeControl($)
  $.ui.invalidate('ui.render')
}

function rewindText() {
  return rewindSec ? Math.floor(rewindSec / 60) + ':' + String(rewindSec % 60).padStart(2, '0') + ' ago' : 'live'
}

async function togglePhoto($) {
  isPhotoShown = !isPhotoShown
  await $.store.set('isPhotoShown', isPhotoShown)
  $.ui.invalidate('ui.render')
}

// The photo for this surface: pixels from skyd's file in kitty and Ghostty,
// its cells elsewhere in the terminal, an embedded JPEG in the Desktop app,
// with the photographer's credit linking to the photo, as Planespotters asks
function photoTree(el, mode) {
  const { Box, Text, Image, Raster, Svg, Link } = el
  if (!photo || photo.none || photo.hex !== selected || !isPhotoShown) return null
  let picture = null
  if (isDesktop) {
    const svg =
      '<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="' + photo.jpeg_width + '" height="' + photo.jpeg_height + '">' +
      '<image width="' + photo.jpeg_width + '" height="' + photo.jpeg_height + '" href="data:image/jpeg;base64,' + photo.jpeg + '" xlink:href="data:image/jpeg;base64,' + photo.jpeg + '"/></svg>'
    picture = Svg({ source: svg, alt: photo.credit, width: photo.jpeg_width, height: photo.jpeg_height })
  } else if (mode === 'image' && photo.file) {
    const rows = Math.max(1, Math.round((photoColumns * photo.height) / photo.width / 2))
    picture = Image({ key: 'photo', source: { file: photo.file, format: 'rgb', width: photo.width, height: photo.height }, columns: photoColumns, rows, alt: photo.credit })
  } else if (photo.cells?.columns === photoColumns) {
    // Cells laid out for another width wait for skyd to lay them out again
    picture = Raster({ key: 'photo', columns: photo.cells.columns, rows: photo.cells.rows, cells: photo.cells.data })
  }
  if (!picture) return null
  return Box({
    flexDirection: 'column',
    children: [picture, photo.link ? Link({ key: 'photo-credit', href: photo.link, label: photo.credit }) : Text({ dimColor: true, children: [photo.credit] })],
  })
}

function photoRows() {
  if (!photo || photo.none || photo.hex !== selected || !isPhotoShown) return 0
  return Math.max(1, Math.round((photoColumns * photo.height) / photo.width / 2)) + 1
}

async function toggleColors($) {
  colors = colors === 'altitude' ? 'status' : 'altitude'
  await $.store.set('colors', colors)
  await writeControl($)
  $.ui.invalidate('ui.render')
}

async function toggleFollow($) {
  isFollowing = !isFollowing && selected !== null
  await writeControl($)
  $.ui.invalidate('ui.render')
}

async function select($, hex) {
  keepOpen()
  selected = selected === hex ? null : hex
  photo = null
  if (!selected) isFollowing = false
  await writeControl($)
  $.ui.invalidate('ui.render')
}

async function toggleView($) {
  keepOpen()
  view = view === 'radar' ? 'window' : 'radar'
  await $.store.set('view', view)
  await writeControl($)
  $.ui.invalidate('ui.render')
}

// "NE", "135" or "auto" as a bearing, or null
function parseFace(text) {
  if (text === 'auto') return 'auto'
  const i = COMPASS.indexOf(text.toUpperCase())
  if (i >= 0) return i * 45
  const deg = Number(text)
  return text !== '' && Number.isFinite(deg) ? ((deg % 360) + 360) % 360 : null
}

// Dragging the radar moves the map under the pointer; dragging the window
// turns it, a full picture's width being its 90° field of view
async function drag($, dx, dy) {
  keepOpen()
  if (view === 'window') {
    const from = typeof face === 'number' ? face : faceNow ?? 180
    face = Math.round((((from - dx * fov) % 360) + 360) % 360)
    // Saved once the turning stops, not on every step of it
    faceSaveTimer?.cancel()
    faceSaveTimer = $.clock.after(800, () => void $.store.set('face', face))
  } else {
    if (!span) return
    // Taking hold of the map lets go of whatever it was following
    isFollowing = false
    pan = { x: pan.x - dx * span.x, y: pan.y + dy * span.y }
  }
  await writeControl($)
  $.ui.invalidate('ui.render')
}

// A double click zooms in a step, centred on where it landed
async function zoomAt($, x, y) {
  if (view !== 'radar' || !span) return
  pan = { x: pan.x + (x - 0.5) * span.x, y: pan.y - (y - 0.5) * span.y }
  await zoom($, -1)
}

async function recenter($) {
  isFollowing = false
  pan = { x: 0, y: 0 }
  await writeControl($)
  $.ui.invalidate('ui.render')
}

// Turns the window a couple of degrees: fine alignment against the real sky
async function nudge($, degrees) {
  keepOpen()
  const from = typeof face === 'number' ? face : faceNow ?? 180
  face = (((from + degrees) % 360) + 360) % 360
  await $.store.set('face', face)
  await writeControl($)
  $.ui.invalidate('ui.render')
}

const CALIBRATE = [
  'Lining the window view up with your real window:',
  '1. /radar face <direction your window looks>, e.g. /radar face SE or 135 (a phone compass helps)',
  '2. /radar fov <degrees>: about 60 for a typical window seen from a desk, 90 if you stand at it',
  '3. When you can see a plane outside, pick it (click it, or 1–5); its label is ringed in yellow',
  '4. Press j or k (2° at a time) or drag until the ring sits where you see the plane',
  'The view then stays put: /radar face auto goes back to facing the busiest airport.',
].join('\n')

async function zoom($, step) {
  keepOpen()
  const index = RANGES_NM.indexOf(rangeNm)
  const next = RANGES_NM[Math.max(0, Math.min(RANGES_NM.length - 1, (index < 0 ? 3 : index) + step))]
  if (next === rangeNm) return
  rangeNm = next
  await $.store.set('rangeNm', rangeNm)
  await writeControl($)
  $.ui.invalidate('ui.render')
}

// ---- commands ------------------------------------------------------------

const HELP = [
  '/radar                 open or close the radar',
  '/radar home            where home is now',
  '/radar home <place>    pin home to a place, such as "Somerville, MA" or "48.85,2.35"',
  '/radar home auto       follow your IP again',
  '/radar range <nm>      how far the radar reaches',
  '/radar zoom in|out     step the range without focusing the pane',
  '/radar center          put home back in the middle after dragging',
  '/radar follow          keep the picked aircraft in the middle (f)',
  '/radar colors altitude|status   color by height, or by arriving/departing (a)',
  '/radar open            open the picked aircraft in Flightradar24 (o)',
  '/radar watch <words>   a toast when something matching shows up, e.g. "any 747 within 20 miles"',
  '/radar watch           your watches; /radar unwatch <n|all> removes them',
  '/radar photos on|off   a photo of the picked aircraft (p)',
  '/radar rewind 10m|live  the sky as it was, up to 30 min back (b back, n forward, l live)',
  '/radar summary on|off  a line under each longer answer: what flew over while Claude worked',
  '/radar view radar|window   top-down, or the sky out of a window (v swaps)',
  '/radar face <dir>      which way the window faces: N, SW, 135 or auto (j/k nudge 2°)',
  '/radar fov <deg>       how wide the window view is; /radar calibrate walks through lining it up',
  '/radar auto events|always|off   drop in when something worth seeing happens (default),',
  '                       5 s into every turn, or never',
  '/radar alerts on|off   a toast when an aircraft is about to pass overhead',
  '/radar alerts quiet 22-7  no alerts between these hours',
  '/radar list            the types and operators you have seen',
  '/radar off             stop polling until the next /radar',
].join('\n')

async function runCommand($, args) {
  const [verb, ...rest] = args.trim().split(/\s+/).filter(Boolean)
  const arg = rest.join(' ')
  if (!verb) {
    // Dropped in by itself: the person wants it to stay
    if (phase === 'auto') {
      phase = 'pinned'
      return { text: 'The radar stays open; /radar again closes it.' }
    }
    if (isOpen) {
      await $.ui.close({ id: PANE })
      return {}
    }
    return openRadar($)
  }
  if (verb === 'help') return { text: HELP }
  if (verb === 'list') {
    lifeList ??= (await $.store.get('lifeList')) ?? { types: {}, operators: {} }
    return { text: lifeListText() }
  }
  if (verb === 'off') {
    if (isOpen) await $.ui.close({ id: PANE })
    withdrawOffer($)
    autoMode = 'off'
    await $.store.set('autoMode', autoMode)
    await stopSkyd($)
    return { text: 'overhead is off: no polling and no dropping in. /radar turns it back on.' }
  }
  if (verb === 'view') {
    if (arg !== 'radar' && arg !== 'window') return { text: 'The view is ' + view + '. /radar view radar|window changes it; v in the pane swaps.' }
    view = arg
    await $.store.set('view', view)
    await writeControl($)
    return { text: view === 'window' ? 'The sky from home, facing ' + faceText() + '.' : 'Top-down radar.' }
  }
  if (verb === 'fov') {
    const deg = Number(arg)
    if (!(deg >= 20 && deg <= 140)) return { text: 'The window view spans ' + fov + '°. /radar fov <20–140>: about 60 for a window seen from a desk.' }
    fov = deg
    await $.store.set('fov', fov)
    await writeControl($)
    return { text: 'The window view spans ' + fov + '° of sky.' }
  }
  if (verb === 'calibrate') {
    if (view !== 'window') {
      view = 'window'
      await $.store.set('view', view)
      await writeControl($)
    }
    return { text: CALIBRATE }
  }
  if (verb === 'face') {
    const next = parseFace(arg)
    if (next === null) return { text: 'The window faces ' + faceText() + '. /radar face N|NE|…|<degrees>|auto' }
    face = next
    await $.store.set('face', face)
    await writeControl($)
    return { text: 'The window faces ' + faceText() + '.' }
  }
  if (verb === 'alerts') {
    const quietMatch = /^quiet (\d{1,2})-(\d{1,2})$/.exec(arg)
    if (quietMatch) {
      quiet = { from: Number(quietMatch[1]) % 24, to: Number(quietMatch[2]) % 24 }
      await $.store.set('quiet', quiet)
      return { text: 'No pass alerts from ' + quiet.from + ':00 to ' + quiet.to + ':00.' }
    }
    if (arg !== 'on' && arg !== 'off') {
      return { text: 'Pass alerts are ' + (isAlerting ? 'on' : 'off') + ', quiet ' + quiet.from + ':00–' + quiet.to + ':00. /radar alerts on|off|quiet 22-7' }
    }
    isAlerting = arg === 'on'
    await $.store.set('isAlerting', isAlerting)
    return { text: isAlerting ? 'A toast when an aircraft is about to pass overhead.' : 'No pass alerts.' }
  }
  if (verb === 'auto') {
    const mode = arg === 'on' ? 'events' : arg
    if (!['events', 'always', 'off'].includes(mode)) {
      return { text: 'The radar drops in: ' + autoModeText() + '. /radar auto events|always|off changes it.' }
    }
    autoMode = mode
    await $.store.set('autoMode', autoMode)
    if (autoMode !== 'always' && phase !== 'pinned') await getOutOfTheWay($)
    return { text: 'The radar drops in ' + autoModeText() + '.' }
  }
  if (verb === 'watch') {
    if (!arg) return { text: watchList() }
    try {
      const rule = await watchFromWords($, arg)
      await addWatch($, arg, rule)
      return { text: 'Watching for ' + (rule.label ?? arg) + ' (' + ruleText(rule) + '). A toast when one shows up.' }
    } catch (error) {
      return { text: "Couldn't set that watch: " + error.message }
    }
  }
  if (verb === 'unwatch') {
    if (arg === 'all') watches = []
    else {
      const n = Number(arg)
      if (!(n >= 1 && n <= watches.length)) return { text: watchList() }
      watches = watches.filter((_, i) => i !== n - 1)
    }
    await $.store.set('watches', watches)
    return { text: watches.length ? watchList() : 'No watches left.' }
  }
  if (verb === 'photos') {
    if (arg !== 'on' && arg !== 'off') return { text: 'Photos are ' + (isPhotoShown ? 'on' : 'off') + '. /radar photos on|off' }
    isPhotoShown = arg === 'on'
    await $.store.set('isPhotoShown', isPhotoShown)
    return { text: isPhotoShown ? 'A photo of the picked aircraft, from Planespotters.' : 'No photos.' }
  }
  if (verb === 'rewind') {
    if (arg === 'live' || arg === '0') {
      await setRewind($, 0)
      return { text: 'The radar is live.' }
    }
    const m = /^(\d+)\s*(m|min|s)?$/.exec(arg)
    if (!m) return { text: 'Rewind up to ' + Math.floor(historySec / 60) + ' min: /radar rewind 10m, or /radar rewind live.' }
    const seconds = Number(m[1]) * (m[2] === 's' ? 1 : 60)
    await setRewind($, seconds)
    return { text: 'Showing the sky ' + rewindText() + '.' }
  }
  if (verb === 'summary') {
    if (arg !== 'on' && arg !== 'off') return { text: 'The line about what flew over while Claude worked is ' + (isSummary ? 'on' : 'off') + '. /radar summary on|off' }
    isSummary = arg === 'on'
    await $.store.set('isSummary', isSummary)
    return { text: isSummary ? 'A line under each longer answer says what flew over while Claude worked.' : 'No lines about the sky under answers.' }
  }
  if (verb === 'colors' || verb === 'colours') {
    if (arg !== 'altitude' && arg !== 'status') return { text: 'Colors by ' + colors + '. /radar colors altitude|status' }
    colors = arg
    await $.store.set('colors', colors)
    await writeControl($)
    return { text: arg === 'altitude' ? 'Colored by altitude: orange low, through green and blue, to magenta at cruise.' : 'Colored by status: green arriving, orange departing, white cruising.' }
  }
  if (verb === 'open') {
    const picked = aircraft.find((a) => a.hex === selected)
    if (!picked) return { text: 'Pick an aircraft first (1–5 in the pane, or click it), then /radar open.' }
    await openLink($, links(picked)[0].href)
    return { text: links(picked).map((l) => l.label + ': ' + l.href).join('\n') }
  }
  if (verb === 'follow') {
    if (!selected) return { text: 'Pick an aircraft first (1–5 in the pane, or click it), then /radar follow.' }
    await toggleFollow($)
    return { text: isFollowing ? 'Following ' + (aircraft.find((a) => a.hex === selected)?.callsign ?? selected) + '.' : 'Stopped following.' }
  }
  if (verb === 'center' || verb === 'centre') {
    await recenter($)
    return { text: 'The radar is centred on home.' }
  }
  if (verb === 'zoom') {
    if (arg !== 'in' && arg !== 'out') return { text: 'Radar range ' + rangeNm + ' nm. /radar zoom in|out' }
    await zoom($, arg === 'in' ? -1 : 1)
    return { text: 'Radar range ' + rangeNm + ' nm.' }
  }
  if (verb === 'range') {
    const nm = Number(arg)
    if (!(nm >= 2 && nm <= 100)) return { text: 'Give a range from 2 to 100 nm, such as /radar range 15.' }
    rangeNm = nm
    await $.store.set('rangeNm', rangeNm)
    await writeControl($)
    return { text: 'Radar range ' + nm + ' nm.' }
  }
  if (verb === 'home') {
    if (!arg) return { text: home ? 'Home is ' + homeText() + ' at ' + home.lat.toFixed(4) + ', ' + home.lon.toFixed(4) + '.' : 'Home is not set yet; /radar finds it.' }
    try {
      if (arg === 'auto') {
        home = null
        await $.store.delete('home')
        await ensureHome($)
        if (skyd) await restartSkyd($)
        return { text: 'Home follows your IP: ' + homeText() + '.' }
      }
      await setHome($, await skydJson($, ['geocode', arg]))
      return { text: 'Home pinned to ' + home.label + ' (' + home.lat.toFixed(4) + ', ' + home.lon.toFixed(4) + '). /radar home auto follows your IP again.' }
    } catch (error) {
      return { text: "Couldn't set home: " + error.message }
    }
  }
  return { text: HELP }
}

// ---- hooks ---------------------------------------------------------------

export function register(on) {
  on('session.start', async ($, e, next) => {
    home = (await $.store.get('home')) ?? null
    rangeNm = (await $.store.get('rangeNm')) ?? DEFAULT_RANGE_NM
    // Before modes, a yes/no that meant "every turn"; events is the kinder reading
    autoMode = (await $.store.get('autoMode')) ?? ((await $.store.get('isAuto')) === true ? 'events' : 'off')
    view = (await $.store.get('view')) ?? 'radar'
    colors = (await $.store.get('colors')) ?? 'altitude'
    isSummary = (await $.store.get('isSummary')) ?? true
    isPhotoShown = (await $.store.get('isPhotoShown')) ?? true
    watches = (await $.store.get('watches')) ?? []
    face = (await $.store.get('face')) ?? 'auto'
    fov = (await $.store.get('fov')) ?? 90
    isAlerting = (await $.store.get('isAlerting')) ?? true
    quiet = (await $.store.get('quiet')) ?? DEFAULT_QUIET
    await $.command.register({
      name: 'radar',
      description: 'Live aircraft overhead, wherever you are',
      argumentHint: '[watch <words> | view radar|window | face <dir> | home <place> | range <nm> | auto events|always|off | alerts on|off | off | help]',
    })
    await $.tool.register({
      name: 'overhead_watch',
      description:
        "Sets a standing watch on the aircraft around the user: they get a toast whenever one matching shows up. Use it when the user asks to be told when a kind of aircraft appears (an A380, anything military, a flight to London, a helicopter overhead). Fill only the fields the request needs; every field given must hold.",
      inputSchema: { type: 'object', properties: RULE_FIELDS, required: ['label'] },
    })
    await $.tool.register({
      name: 'overhead_now',
      description: TOOL_DESCRIPTION,
      inputSchema: {
        type: 'object',
        properties: { limit: { type: 'number', description: 'How many of the nearest airborne aircraft to describe (default 8, at most 25)' } },
      },
    })
    return next(e)
  })

  on('command.run', { command: 'radar' }, async ($, e) => runCommand($, e.args))

  on('ui.close', async ($, e, next) => {
    if (e.id !== PANE) return next(e)
    if (e.origin?.kind === 'person' && isTurnRunning) isDismissed = true
    cancelTimer()
    eventTimer?.cancel()
    eventTimer = null
    phase = 'idle'
    isOpen = false
    fpsTimer?.cancel()
    fpsTimer = null
    frame = null
    // Keep polling, slowly, so the status line stays live
    await writeControl($)
    return next(e)
  })

  on('turn.start', async ($, e, next) => {
    isTurnRunning = true
    isDismissed = false
    armDropIn($)
    // A fresh tally for the line under the answer, if anything's polling
    if (!turnSky) turnSky = { startedAt: await $.clock.now(), seen: new Set(), closest: null, newTypes: [], interesting: [], emergencies: [] }
    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    // A subagent's turn ending isn't Claude being done
    if (e.agentId) return next(e)
    isTurnRunning = false
    if (phase === 'waiting' || phase === 'offered') withdrawOffer($)
    else if (phase === 'auto') await $.ui.close({ id: PANE })
    const sky = turnSky
    turnSky = null
    const result = await next(e)
    // Worth a line only after a while, and only if the sky was being watched
    const minutes = sky ? ((await $.clock.now()) - sky.startedAt) / 60000 : 0
    if (!isSummary || !sky || minutes < 0.5 || sky.seen.size === 0 || e.isAborted) return result
    turnSky = sky
    const text = turnSkyText(minutes)
    turnSky = null
    return { ...result, text }
  })

  on('tool.check', async ($, e, next) => {
    const result = await next(e)
    // In auto mode an ask can go to the classifier instead of the person;
    // getting out of the way anyway costs a moment, hiding a prompt costs more
    if (e.tool_use_id && result.decision === 'ask') await getOutOfTheWay($)
    return result
  })

  on('tool.call', { tool: TOOL }, async ($, e) => ({ result: await answerTool($, e.input ?? e) }))

  on('tool.call', { tool: 'mcp__overhead__overhead_watch' }, async ($, e) => {
    const rule = cleanRule(e.input ?? e)
    if (!rule) return { result: 'That watch has nothing to look for: give at least one field besides the label.' }
    await addWatch($, rule.label ?? ruleText(rule), rule)
    return { result: 'Watching for ' + (rule.label ?? ruleText(rule)) + ' (' + ruleText(rule) + '). The user gets a toast when one shows up; /radar watch lists watches.' }
  })

  on('tool.call', async ($, e, next) => {
    if (e.tool === 'AskUserQuestion') await getOutOfTheWay($)
    const result = await next(e)
    // Once an answer lets Claude carry on, drop back in
    armDropIn($)
    return result
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    if (phase !== 'offered') return next(e)
    const { Box, Button } = $.ui.resolve(e)
    // Keep whatever other mods show in the band
    const others = await next(e)
    return Box({
      flexDirection: 'column',
      children: [
        Button({ key: 'watch', label: 'Watch the sky while Claude works', hotkey: '1', plain: true, onPress: () => acceptOffer($) }),
        ...(others ? [others] : []),
      ],
    })
  })

  on('ui.message', async ($, e) => {
    if (e.element !== 'input') return {}
    const data = e.data ?? {}
    if (Array.isArray(data.drag)) {
      await drag($, Number(data.drag[0]) || 0, Number(data.drag[1]) || 0)
    } else if (typeof data.double === 'number') {
      await zoomAt($, Number(data.x), Number(data.y))
    } else if (typeof data.click === 'number') {
      click = { n: data.click, x: Number(data.x), y: Number(data.y) }
      await writeControl($)
    }
    return {}
  })

  on('session.end', async ($, e, next) => {
    await stopSkyd($)
    return next(e)
  })

  on('ui.render', { component: 'Pane' }, async ($, e, next) => {
    if (e.requestId !== PANE) return next(e)
    const { Box, Text, Image, Raster, Button, Client, Svg, Link } = $.ui.resolve(e)
    if (e.surface !== 'terminal' && e.surface !== 'desktop') return Text({ children: ['overhead draws in the terminal and the Desktop app.'] })
    if (isDesktop !== (e.surface === 'desktop')) {
      isDesktop = e.surface === 'desktop'
      void writeControl($)
    }

    // The Desktop app's skyd draws nothing; it only polls
    const mode = isDesktop ? 'raster' : await drawMode($)
    // The photo takes about half the pane's width, at most 48 cells
    const nextPhotoColumns = Math.max(16, Math.min(48, Math.floor(e.props.bodyColumns * 0.5)))
    if (nextPhotoColumns !== photoColumns) {
      photoColumns = nextPhotoColumns
      void writeControl($)
    }
    const listed = airborne().slice(0, LISTED)
    const picked = aircraft.find((a) => a.hex === selected)
    // Facts, the list, the picked one's detail, the buttons, any error
    // Facts, the list, the picked one's detail and any error as they wrap,
    // the keys (two lines when narrow) and the hint
    const linesOf = (text) => Math.max(1, Math.ceil(text.length / Math.max(20, e.props.bodyColumns)))
    const footerRows =
      1 + (ops ? linesOf(opsText()) : 0) + listed.length + (picked ? linesOf(detail(picked)) + 1 + photoRows() : 0) + (lastError ? linesOf(lastError) : 0) + (e.props.bodyColumns < 90 ? 2 : 1) + 1
    const maxRows = e.props.placement === 'dock' && e.props.scroll?.bodyRows ? e.props.scroll.bodyRows - footerRows : null
    const nextSize = fit(mode, e.props.bodyColumns, maxRows)
    const isResized = size?.columns !== nextSize.columns || size?.rows !== nextSize.rows
    size = nextSize
    if (home && isOpen) {
      if (!skyd && !isBackingOff) void runSkyd($, mode)
      else if (skydMode !== mode) void restartSkyd($, mode)
      else if (isResized) {
        // skyd lays out the next frame for the new size; until then, the
        // mounted Raster would refuse cells of another size
        cells = null
        void writeControl($)
      }
    }

    const facing = faceNow ?? (typeof face === 'number' ? face : null)
    const viewFacts =
      view === 'window' && facing != null
        ? ['facing ' + Math.round(facing) + '° ' + COMPASS[Math.round(facing / 45) % 8] + (face === 'auto' ? ' (auto)' : ''), fov + '° wide']
        : [rangeNm + ' nm']
    const facts = [homeText(), ...viewFacts, aircraft.length + ' aircraft', ...(rewindSec ? ['⏪ ' + rewindText()] : []), ...(isDesktop ? [] : [fps + ' fps'])].join(' · ')
    const footer = Box({
      flexDirection: 'column',
      children: [
        Text({ dimColor: true, children: [facts] }),
        ...(opsParts()
          ? [
              Box({
                flexDirection: 'row',
                flexWrap: 'wrap',
                columnGap: 1,
                children: [
                  Text({ bold: true, children: [opsParts().airport] }),
                  ...(opsParts().category ? [Text({ color: CATEGORY_COLORS[opsParts().category] ?? 'white', bold: true, children: [opsParts().category] })] : []),
                  ...(opsParts().rest.length ? [Text({ dimColor: true, children: ['· ' + opsParts().rest.join(' · ')] })] : []),
                ],
              }),
            ]
          : []),
        ...listed.map((a, i) =>
          Button({
            key: 'pick-' + a.hex,
            label: (a.hex === selected ? '▸ ' : '') + describe(a),
            hotkey: String(i + 1),
            plain: true,
            onPress: () => select($, a.hex),
          }),
        ),
        ...(picked ? [Text({ color: 'yellow', wrap: 'wrap', children: [detail(picked)] })] : []),
        ...(picked
          ? [
              Box({
                flexDirection: 'row',
                flexWrap: 'wrap',
                columnGap: 2,
                children: [
                  ...links(picked).map((l) => Link({ key: 'link-' + l.key, href: l.href, label: l.label })),
                  Button({ key: 'open', label: 'open in Flightradar24', hotkey: 'o', plain: true, onPress: () => openLink($, links(picked)[0].href) }),
                  ...(photo && !photo.none ? [Button({ key: 'photo', label: isPhotoShown ? 'hide photo' : 'show photo', hotkey: 'p', plain: true, onPress: () => togglePhoto($) })] : []),
                ],
              }),
              ...[photoTree($.ui.resolve(e), mode)].filter(Boolean),
            ]
          : []),
        // Being asked to slow down is routine, not an error: the map carries on
        ...(lastError
          ? [
              lastError.startsWith('adsb.lol asked')
                ? Text({ dimColor: true, wrap: 'wrap', children: ['⏳ ' + lastError] })
                : Text({ color: 'yellow', wrap: 'wrap', children: [lastError] }),
            ]
          : []),
        // The keys, onto a second line when the pane is narrow
        Box({
          flexDirection: 'row',
          flexWrap: 'wrap',
          columnGap: 2,
          children: [
            Button({ key: 'in', label: 'zoom in', hotkey: 'z', plain: true, onPress: () => zoom($, -1) }),
            Button({ key: 'out', label: 'zoom out', hotkey: 'x', plain: true, onPress: () => zoom($, 1) }),
            ...(pan.x || pan.y ? [Button({ key: 'center', label: 'center', hotkey: 'c', plain: true, onPress: () => recenter($) })] : []),
            ...(selected && view === 'radar'
              ? [Button({ key: 'follow', label: isFollowing ? 'stop following' : 'follow', hotkey: 'f', plain: true, onPress: () => toggleFollow($) })]
              : []),
            ...(!isDesktop && historySec > rewindSec + 30
              ? [Button({ key: 'back', label: '1 min back', hotkey: 'b', plain: true, onPress: () => setRewind($, rewindSec + 60) })]
              : []),
            ...(rewindSec
              ? [
                  Button({ key: 'forward', label: '1 min on', hotkey: 'n', plain: true, onPress: () => setRewind($, rewindSec - 60) }),
                  Button({ key: 'live', label: 'live', hotkey: 'l', plain: true, onPress: () => setRewind($, 0) }),
                ]
              : []),
            ...(view === 'window' && !isDesktop
              ? [
                  Button({ key: 'left', label: 'turn left', hotkey: 'j', plain: true, onPress: () => nudge($, -2) }),
                  Button({ key: 'right', label: 'turn right', hotkey: 'k', plain: true, onPress: () => nudge($, 2) }),
                ]
              : []),
            ...(view === 'radar'
              ? [Button({ key: 'colors', label: colors === 'altitude' ? 'status colors' : 'altitude colors', hotkey: 'a', plain: true, onPress: () => toggleColors($) })]
              : []),
            ...(isDesktop
              ? []
              : [Button({ key: 'view', label: view === 'radar' ? 'window view' : 'radar view', hotkey: 'v', plain: true, onPress: () => toggleView($) })]),
          ],
        }),
        Text({
          dimColor: true,
          wrap: 'wrap',
          // Hotkeys only reach a focused pane; the prompt keeps them otherwise
          children: [e.props.isFocused ? 'esc: back to the prompt · /radar help' : 'ctrl+x tab or click here for keys · /radar help'],
        }),
      ],
    })

    let picture = Text({ children: [home ? 'Starting the radar…' : 'Finding where you are…'] })
    if (isDesktop) {
      if (received) picture = Svg({ source: radarSvg(), alt: 'Radar of ' + aircraft.length + ' aircraft around home', width: SVG_W, height: SVG_H })
      return Box({ flexDirection: 'column', children: [picture, footer] })
    }
    if (mode === 'image' && frame) {
      picture = Image({ key: 'view', source: shmSource(frame), columns: size.columns, rows: size.rows, alt: 'radar' })
    } else if (mode === 'raster' && cells) {
      picture = Raster({ key: 'view', columns: size.columns, rows: size.rows, cells })
    }
    // Clicks on the picture pick the aircraft under them
    const overlay =
      cells || frame
        ? [
            Box({
              position: 'absolute',
              top: 0,
              left: 0,
              children: [Client({ key: 'input', module: './input.js', width: size.columns, height: size.rows, props: { columns: size.columns, rows: size.rows } })],
            }),
          ]
        : []
    return Box({ flexDirection: 'column', children: [picture, ...overlay, footer] })
  })
}
