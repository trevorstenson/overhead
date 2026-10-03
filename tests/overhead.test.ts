import { expect, mock, test } from 'claude-code/testing'

const SOMERVILLE = { lat: 42.3876, lon: -71.0995, label: 'Somerville, Massachusetts, US', source: 'place' }

const AIRCRAFT = [
  { hex: 'a575e0', callsign: null, type: 'A21N', registration: 'N451AN', alt_ft: null, gs_kt: 0, track_deg: null, vrate_fpm: 0, status: 'ground', distance_nm: 3.5, bearing_deg: 104 },
  { hex: 'a39fe4', callsign: 'JBU1786', type: 'A21N', registration: 'N3322J', alt_ft: 8375, gs_kt: 282.9, track_deg: 105.2, vrate_fpm: -832, status: 'arrival', distance_nm: 6.2, bearing_deg: 321 },
]

test('/radar help lists the commands', async ($, on) => {
  mock.store(on)
  const answer = await $.command.run({ command: 'radar', args: 'help' })
  expect(answer.text).toContain('/radar home <place>')
})

test('/radar range refuses nonsense and keeps a good value', async ($, on) => {
  mock.store(on)
  const bad = await $.command.run({ command: 'radar', args: 'range 500' })
  expect(bad.text).toContain('2 to 100')
  const good = await $.command.run({ command: 'radar', args: 'range 20' })
  expect(good.text).toBe('Radar range 20 nm.')
})

test('/radar home <place> pins home through skyd geocode', async ($, on) => {
  mock.store(on)
  mock.clock(on, { now: 1000 })
  on('fs.exists', async () => ({ value: true }))
  const asked: string[][] = []
  on('process.run', async (_$, e) => {
    asked.push([...e.argv])
    return { value: { exitCode: 0, stdout: JSON.stringify(SOMERVILLE) + '\n', stderr: '' } }
  })
  const answer = await $.command.run({ command: 'radar', args: 'home Somerville, MA' })
  expect(asked[0].slice(1)).toEqual(['geocode', 'Somerville, MA'])
  expect(answer.text).toContain('Home pinned to Somerville, Massachusetts, US')
  const now = await $.command.run({ command: 'radar', args: 'home' })
  expect(now.text).toBe('Home is Somerville, Massachusetts, US at 42.3876, -71.0995.')
})

test('/radar home reports a place skyd cannot find', async ($, on) => {
  mock.store(on)
  on('fs.exists', async () => ({ value: true }))
  on('process.run', async () => ({ value: { exitCode: 2, stdout: '@error no place called Atlantis\n', stderr: '' } }))
  const answer = await $.command.run({ command: 'radar', args: 'home Atlantis' })
  expect(answer.text).toBe("Couldn't set home: no place called Atlantis")
})

test('opening the radar finds home from the IP and shows the nearest airborne aircraft', async ($, on) => {
  mock.store(on)
  const clock = mock.clock(on, { now: 1000 })
  mock.env(on, { TERM_PROGRAM: 'WarpTerminal' })
  on('fs.exists', async () => ({ value: true }))
  on('fs.write', async () => ({ value: undefined }))
  on('ui.open', async () => ({ value: { isPlaced: true } }))
  on('process.run', async () => ({
    value: { exitCode: 0, stdout: JSON.stringify({ ...SOMERVILLE, source: 'ip' }) + '\n', stderr: '' },
  }))
  const statuses: (string | undefined)[] = []
  on('ui.status', async (_$, e) => {
    statuses.push(e.text)
    return { value: undefined }
  })
  const spawned: string[][] = []
  on('process.spawn', async function* (_$, e) {
    spawned.push([...e.argv])
    yield { stream: 'stdout' as const, text: '@ready\n@aircraft ' + JSON.stringify(AIRCRAFT) + '\n' }
    return { value: { code: 0, signal: null } }
  })

  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await clock.settle()

  expect(spawned[0]).toEqual(expect.arrayContaining(['run', '--lat', '42.3876', '--mode', 'cells', '--columns', '100']))
  expect(statuses).toContain('✈ JBU1786 · A21N · 6.2 nm NW · 8,375 ft')
  expect((await ui.find({ type: 'Text', text: /near Somerville/ }))?.text).toContain('(from your IP)')
})

test('pressing an aircraft in the list selects it on the radar and shows its route', async ($, on) => {
  mock.store(on)
  const clock = mock.clock(on, { now: 1000 })
  mock.env(on, { TERM_PROGRAM: 'WarpTerminal' })
  on('fs.exists', async () => ({ value: true }))
  const controls: string[] = []
  on('fs.write', async (_$, e) => {
    controls.push(e.text)
    return { value: undefined }
  })
  on('ui.open', async () => ({ value: { isPlaced: true } }))
  on('ui.status', async () => ({ value: undefined }))
  on('process.run', async () => ({ value: { exitCode: 0, stdout: JSON.stringify(SOMERVILLE) + '\n', stderr: '' } }))
  const routed = AIRCRAFT.map((a) =>
    a.callsign === 'JBU1786'
      ? {
          ...a,
          model: 'Airbus A321-271NX',
          owner: 'JetBlue Airways',
          route: {
            from: { code: 'PIT', city: 'Pittsburgh', lat: 40.49, lon: -80.23 },
            to: { code: 'BOS', city: 'Boston', lat: 42.36, lon: -71.01 },
            airline: null,
          },
        }
      : a,
  )
  on('process.spawn', async function* () {
    yield { stream: 'stdout' as const, text: '@aircraft ' + JSON.stringify(routed) + '\n' }
    return { value: { code: 0, signal: null } }
  })

  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await clock.settle()

  // Ground traffic isn't listed; the airborne arrival is, with its route
  const row = await ui.find({ key: 'pick-a39fe4' })
  expect(row?.text).toContain('JBU1786 · A21N · PIT→BOS')
  expect(await ui.find({ key: 'pick-a575e0' })).toBeUndefined()

  await ui.press({ key: 'pick-a39fe4' })
  expect(controls.at(-1)).toContain('select a39fe4')
  expect((await ui.find({ type: 'Text', text: /Pittsburgh → Boston/ }))?.text).toBe(
    'JBU1786 · JetBlue Airways · Airbus A321-271NX N3322J · Pittsburgh → Boston · 8,375 ft ↓ 832 fpm · 283 kt · 6.2 nm NW',
  )

  // Pressing it again lets go
  await ui.press({ key: 'pick-a39fe4' })
  expect(controls.at(-1)).toContain('select -')
})

// Whether the next skyd a lifecycle world starts reports a pass overhead
let passOverhead = false
// Lines the next skyd a lifecycle world starts prints first
let skydSays = ''

// A Raster's cells, every one a blank on the default colors
function blankCells(columns: number, rows: number) {
  const words = new Uint32Array(columns * rows * 3)
  for (let i = 0; i < words.length; i += 3) words.set([0x20, 0x01000000, 0x01000000], i)
  const bytes = new Uint8Array(words.buffer)
  let binary = ''
  for (const b of bytes) binary += String.fromCharCode(b)
  return btoa(binary)
}

// The world a lifecycle test runs in: a terminal session where skyd answers
// with nothing, and every pane open and close recorded
function lifecycleWorld($: any, on: any, store: Record<string, unknown> = { isAuto: true, home: SOMERVILLE }) {
  mock.store(on, store)
  const clock = mock.clock(on, { now: 1000 })
  mock.env(on, { TERM_PROGRAM: 'WarpTerminal' })
  const events: string[] = []
  // What the mod wrote to skyd's control file, newest last
  const controls: string[] = []
  on('fs.exists', async () => ({ value: true }))
  on('fs.write', async (_$: unknown, e: { text: string }) => {
    controls.push(e.text)
    return { value: undefined }
  })
  on('session.surfaces', async () => ({ value: ['terminal'] }))
  on('command.register', async () => ({ value: undefined }))
  on('tool.register', async () => ({ value: { tool: 'mcp__overhead__overhead_now' } }))
  on('ui.log', async () => ({ value: undefined }))
  // What the engine answers: the event's own turn
  on('turn.start', async (_$: unknown, e: { turnId: string }) => ({ turnId: e.turnId }))
  on('turn.complete', async (_$: unknown, e: { answer: string }) => ({ text: e.answer }))
  on('session.start', async (_$: unknown, e: { cwd: string }) => ({ cwd: e.cwd }))
  on('ui.status', async () => ({ value: undefined }))
  on('ui.open', async (_$: unknown, e: { focus?: boolean }) => {
    events.push(e.focus ? 'open:focus' : 'open')
    return { value: { isPlaced: true } }
  })
  on('ui.close', async (_$: unknown, e: { origin?: { kind: string } }) => {
    events.push('close:' + (e.origin?.kind ?? '?'))
    return { value: undefined }
  })
  on('process.spawn', async function* () {
    if (skydSays) {
      yield { stream: 'stdout' as const, text: skydSays }
      // Like the real one, keep running until the mod stops it
      await new Promise(() => {})
    }
    if (passOverhead) {
      const pass = { hex: 'a39fe4', callsign: 'JBU1786', type: 'A21N', model: 'Airbus A321-271NX', alt_ft: 3100, in_s: 40, closest_nm: 0.2, route: null }
      yield { stream: 'stdout' as const, text: '@overhead ' + JSON.stringify(pass) + '\n' }
    }
    return { value: { code: 0, signal: null } }
  })
  return { clock, events, controls }
}

test('the radar drops in after five seconds of a turn and leaves when it ends', async ($, on) => {
  const { clock, events } = lifecycleWorld($, on)
  await $.session.start({ cwd: '/tmp' } as never)
  await $.turn.start({ text: 'refactor it', turnId: 't1' } as never)
  await clock.advance(4000)
  expect(events).toEqual([])
  await clock.advance(1000)
  expect(events).toEqual(['open'])
  await $.turn.complete({ reason: 'end_turn', answer: 'done', durationMs: 1, isAborted: false, turnId: 't1' } as never)
  expect(events.at(-1)).toMatch(/^close/)
})

test('a short turn ends before the radar drops in', async ($, on) => {
  const { clock, events } = lifecycleWorld($, on)
  await $.session.start({ cwd: '/tmp' } as never)
  await $.turn.start({ text: 'hi', turnId: 't1' } as never)
  await clock.advance(2000)
  await $.turn.complete({ reason: 'end_turn', answer: 'hello', durationMs: 1, isAborted: false, turnId: 't1' } as never)
  await clock.advance(10000)
  expect(events).toEqual([])
})

test('nothing drops in for someone who never used /radar', async ($, on) => {
  const { clock, events } = lifecycleWorld($, on, { home: SOMERVILLE })
  await $.session.start({ cwd: '/tmp' } as never)
  await $.turn.start({ text: 'refactor it', turnId: 't1' } as never)
  await clock.advance(10000)
  expect(events).toEqual([])
})

test('a permission prompt sends a dropped-in radar away', async ($, on) => {
  const { clock, events } = lifecycleWorld($, on)
  on('tool.check', async () => ({ decision: 'ask' }))
  await $.session.start({ cwd: '/tmp' } as never)
  await $.turn.start({ text: 'delete the build', turnId: 't1' } as never)
  await clock.advance(5000)
  expect(events).toEqual(['open'])
  await $.tool.check({ tool: 'Bash', tool_use_id: 'tu1', input: { command: 'rm -rf build' } } as never)
  expect(events.at(-1)).toMatch(/^close/)
})

test('a radar the person opened stays through the end of a turn', async ($, on) => {
  const { clock, events } = lifecycleWorld($, on)
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  // Asked for, so it takes the keys; dropping in by itself never does
  expect(events).toEqual(['open:focus'])
  await $.turn.start({ text: 'refactor it', turnId: 't1' } as never)
  await clock.advance(10000)
  await $.turn.complete({ reason: 'end_turn', answer: 'done', durationMs: 1, isAborted: false, turnId: 't1' } as never)
  expect(events).toEqual(['open:focus'])
})

test('overhead_now answers Claude from live data, starting skyd when nothing was polling', async ($, on) => {
  mock.store(on, { home: SOMERVILLE })
  const clock = mock.clock(on, { now: 1000 })
  mock.env(on, { TERM_PROGRAM: 'WarpTerminal' })
  on('fs.exists', async () => ({ value: true }))
  on('fs.write', async () => ({ value: undefined }))
  on('ui.status', async () => ({ value: undefined }))
  on('ui.log', async () => ({ value: undefined }))
  on('command.register', async () => ({ value: undefined }))
  on('tool.register', async () => ({ value: { tool: 'mcp__overhead__overhead_now' } }))
  on('session.start', async (_$: unknown, e: { cwd: string }) => ({ cwd: e.cwd }))
  let spawns = 0
  on('process.spawn', async function* () {
    spawns += 1
    yield { stream: 'stdout' as const, text: '@aircraft ' + JSON.stringify(AIRCRAFT) + '\n' }
    return { value: { code: 0, signal: null } }
  })
  await $.session.start({ cwd: '/tmp' } as never)

  const calling = $.tool.call({ tool: 'mcp__overhead__overhead_now', limit: 3 } as never)
  // The tool waits for skyd's first answer on the clock
  await clock.advance(500)
  const answer = await calling
  const text = String((answer as { result?: unknown }).result)
  expect(spawns).toBe(1)
  expect(text).toContain('Home: Somerville, Massachusetts, US')
  expect(text).toContain('1 airborne and 1 on the ground')
  expect(text).toContain('1. JBU1786')
})

// Opens the radar in a world where skyd reports one pass overhead, at a
// local time of day, and returns the toasts it raised
async function toastsForPassAt($: any, on: any, hour: number) {
  const toasts: string[] = []
  const { clock } = lifecycleWorld($, on, { home: SOMERVILLE })
  await clock.set(new Date(2026, 9, 2, hour, 30).getTime())
  on('ui.toast', async (_$: unknown, e: { text: string }) => {
    toasts.push(e.text)
    return { value: undefined }
  })
  on('process.run', async () => ({ value: { exitCode: 0, stdout: JSON.stringify(SOMERVILLE) + '\n', stderr: '' } }))
  passOverhead = true
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await clock.settle()
  passOverhead = false
  return toasts
}

test('a pass overhead makes a toast', async ($, on) => {
  const toasts = await toastsForPassAt($, on, 14)
  expect(toasts).toEqual(['✈ JBU1786 · Airbus A321-271NX overhead in 40 s at 3,100 ft'])
})

test('no pass toasts in quiet hours', async ($, on) => {
  expect(await toastsForPassAt($, on, 23)).toEqual([])
})

test('v swaps to the window view, and /radar face turns it', async ($, on) => {
  const { clock, controls } = lifecycleWorld($, on)
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await clock.settle()
  await ui.press({ key: 'view' })
  expect(controls.at(-1)).toContain('view window face auto')
  expect((await ui.find({ key: 'view', type: 'Button' }))?.text).toContain('radar view')

  const turned = await $.command.run({ command: 'radar', args: 'face sw' })
  expect(turned.text).toBe('The window faces SW (225°).')
  expect(controls.at(-1)).toContain('face 225')
  expect((await $.command.run({ command: 'radar', args: 'face sideways' })).text).toContain('/radar face N|NE')
})

test('a click on the picture goes to skyd, and its answer picks the aircraft', async ($, on) => {
  const { clock, controls } = lifecycleWorld($, on)
  skydSays = '@cells 100 28 ' + blankCells(100, 28) + '\n@aircraft ' + JSON.stringify(AIRCRAFT) + '\n'
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await clock.settle()

  // As the overlay posts a left click a quarter of the way across
  await ui.post({ click: 1, x: 0.25, y: 0.5 }, { in: 'input' })
  expect(controls.at(-1)).toContain('click 1 0.2500 0.5000')
  skydSays = ''
})

test('skyd naming the aircraft under a click selects it', async ($, on) => {
  const { clock, controls } = lifecycleWorld($, on)
  skydSays = '@aircraft ' + JSON.stringify(AIRCRAFT) + '\n@select a39fe4\n'
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await clock.settle()
  expect(controls.at(-1)).toContain('select a39fe4')
  expect((await ui.find({ key: 'pick-a39fe4' }))?.text).toMatch(/^▸ JBU1786/)
  skydSays = ''
})

test('the life list fills quietly at first, then toasts a new type', async ($, on) => {
  const toasts: string[] = []
  const { clock } = lifecycleWorld($, on, { isAuto: true, home: SOMERVILLE, isAlerting: true })
  await clock.set(new Date(2026, 9, 2, 14, 0).getTime())
  on('ui.toast', async (_$: unknown, e: { text: string }) => {
    toasts.push(e.text)
    return { value: undefined }
  })
  const jumbo = { ...AIRCRAFT[1], hex: 'abc747', callsign: 'GTI8', type: 'B744', model: 'Boeing 747-47UF', owner: 'Atlas Air' }
  skydSays =
    '@aircraft ' + JSON.stringify(AIRCRAFT) + '\n' + '@aircraft ' + JSON.stringify([...AIRCRAFT, jumbo]) + '\n'
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await clock.settle()
  skydSays = ''
  expect(toasts).toEqual(['✦ New for your list: Boeing 747-47UF · Atlas Air (GTI8)'])
  const list = await $.command.run({ command: 'radar', args: 'list' })
  expect(list.text).toContain('2 types and 1 operators seen from home.')
  expect(list.text).toContain('B744 Boeing 747-47UF · seen 1')
})

test('without a local build, skyd is downloaded for this machine and checked', async ($, on) => {
  mock.store(on, { home: SOMERVILLE })
  mock.clock(on, { now: 1000 })
  let unpacked = false
  on('fs.exists', async (_$: unknown, e: { path: string }) => ({ value: unpacked && e.path.endsWith('/dist/skyd') }))
  on('fs.read', async () => ({ value: JSON.stringify({ version: '0.2.0' }) }))
  on('ui.invalidate', async () => ({ value: undefined }))
  const ran: string[] = []
  on('process.run', async (_$: unknown, e: { argv: string[] }) => {
    ran.push(e.argv.join(' '))
    const [cmd] = e.argv
    if (cmd === 'uname') return { value: { exitCode: 0, stdout: 'Darwin arm64\n', stderr: '' } }
    if (cmd === 'curl' && e.argv.at(-1)!.endsWith('.sha256')) return { value: { exitCode: 0, stdout: 'abc123  skyd.tar.gz\n', stderr: '' } }
    if (cmd === 'shasum') return { value: { exitCode: 0, stdout: 'abc123  /x/skyd-download.tar.gz\n', stderr: '' } }
    if (cmd === 'tar') unpacked = true
    if (e.argv[1] === 'geocode') return { value: { exitCode: 0, stdout: JSON.stringify(SOMERVILLE) + '\n', stderr: '' } }
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  const answer = await $.command.run({ command: 'radar', args: 'home Somerville, MA' })
  expect(answer.text).toContain('Home pinned to Somerville')
  expect(ran.some((r) => r.includes('/v0.2.0/skyd-macos-arm64.tar.gz'))).toBe(true)
  expect(ran.at(-1)).toMatch(/dist\/skyd geocode Somerville, MA$/)
})

test('a download that fails its checksum is thrown away', async ($, on) => {
  mock.store(on)
  on('fs.exists', async () => ({ value: false }))
  on('fs.read', async () => ({ value: JSON.stringify({ version: '0.2.0' }) }))
  on('ui.invalidate', async () => ({ value: undefined }))
  const ran: string[] = []
  on('process.run', async (_$: unknown, e: { argv: string[] }) => {
    ran.push(e.argv[0])
    if (e.argv[0] === 'uname') return { value: { exitCode: 0, stdout: 'Darwin arm64\n', stderr: '' } }
    if (e.argv[0] === 'curl' && e.argv.at(-1)!.endsWith('.sha256')) return { value: { exitCode: 0, stdout: 'aaa\n', stderr: '' } }
    if (e.argv[0] === 'shasum') return { value: { exitCode: 0, stdout: 'bbb  x\n', stderr: '' } }
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  const answer = await $.command.run({ command: 'radar', args: 'home Lyon' })
  expect(answer.text).toContain("didn't match its checksum")
  expect(ran).not.toContain('tar')
})

test('the Desktop app gets an SVG radar, and skyd only polls', async ($, on) => {
  const { clock, controls } = lifecycleWorld($, on)
  skydSays = '@aircraft ' + JSON.stringify(AIRCRAFT) + '\n'
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'desktop',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await clock.settle()
  skydSays = ''
  const svg = await ui.find({ type: 'Svg' })
  expect(svg).toBeDefined()
  expect(controls.at(-1)).toContain('paused 1')
  expect(await ui.find({ key: 'pick-a39fe4' })).toBeDefined()
  expect(await ui.find({ key: 'view', type: 'Button' })).toBeUndefined()
})

test('/radar zoom steps the range from the prompt', async ($, on) => {
  const { controls } = lifecycleWorld($, on)
  await $.session.start({ cwd: '/tmp' } as never)
  expect((await $.command.run({ command: 'radar', args: 'zoom in' })).text).toBe('Radar range 8 nm.')
  expect((await $.command.run({ command: 'radar', args: 'zoom out' })).text).toBe('Radar range 12 nm.')
  expect((await $.command.run({ command: 'radar', args: 'zoom sideways' })).text).toContain('/radar zoom in|out')
})

// Opens the radar with skyd drawing a 100×28 Raster that spans 40 × 22.4 nm,
// facing 110° in the window, and returns the mounted pane
async function openWithMap($: any, on: any) {
  const world = lifecycleWorld($, on)
  skydSays = '@cells 100 28 ' + blankCells(100, 28) + '\n@scale 40 22.4\n@face 110\n@aircraft ' + JSON.stringify(AIRCRAFT) + '\n'
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await world.clock.settle()
  skydSays = ''
  return { ...world, ui }
}

test('dragging the radar moves the map under the pointer, and c centres it again', async ($, on) => {
  const { ui, controls } = await openWithMap($, on)
  await ui.pointer({ type: 'down', button: 'left', x: 50, y: 14, in: 'input' })
  await ui.pointer({ type: 'move', button: 'left', x: 60, y: 14, in: 'input' })
  await ui.pointer({ type: 'up', button: 'left', x: 60, y: 14, in: 'input' })
  // A tenth of the picture to the right is 4 nm: the middle is now 4 nm west
  expect(controls.at(-1)).toContain('pan -4.000 0.000')
  expect(controls.at(-1)).not.toContain('click')
  await ui.press({ key: 'center' })
  expect(controls.at(-1)).toContain('pan 0.000 0.000')
})

test('a click that barely moves is a click, and a quick second one zooms in there', async ($, on) => {
  const { ui, controls } = await openWithMap($, on)
  await ui.pointer({ type: 'down', button: 'left', x: 75, y: 7, in: 'input' })
  await ui.pointer({ type: 'up', button: 'left', x: 75, y: 7, in: 'input' })
  expect(controls.at(-1)).toContain('click 1 0.7550 0.2679')
  await ui.pointer({ type: 'down', button: 'left', x: 75, y: 7, in: 'input' })
  await ui.pointer({ type: 'up', button: 'left', x: 75, y: 7, in: 'input' })
  // A step in from 12 nm, centred a quarter right and a quarter up
  const last = controls.at(-1)!
  expect(last).toContain('range 8')
  expect(last).toMatch(/pan 10\.200 5\.200/)
})

test('dragging the window view turns it', async ($, on) => {
  const { ui, controls } = await openWithMap($, on)
  await ui.press({ key: 'view' })
  await ui.pointer({ type: 'down', button: 'left', x: 50, y: 14, in: 'input' })
  await ui.pointer({ type: 'move', button: 'left', x: 60, y: 14, in: 'input' })
  await ui.pointer({ type: 'up', button: 'left', x: 60, y: 14, in: 'input' })
  // A tenth of the 90° view to the right: looking 9° further left
  expect(controls.at(-1)).toContain('view window face 101')
})

const MAYDAY = { ...AIRCRAFT[1], hex: 'e77001', callsign: 'N7700X', type: 'C172', squawk: '7700', emergency: 'emergency', distance_nm: 9.1 }

test('an aircraft in an emergency goes to the top, takes the status line, and toasts at any hour', async ($, on) => {
  const toasts: string[] = []
  const world = lifecycleWorld($, on)
  await world.clock.set(new Date(2026, 9, 2, 23, 30).getTime())
  on('ui.toast', async (_$: unknown, e: { text: string }) => {
    toasts.push(e.text)
    return { value: undefined }
  })
  const alert = { kind: 'emergency', hex: 'e77001', callsign: 'N7700X', type: 'C172', model: 'Cessna 172', squawk: '7700', what: 'emergency', alt_ft: 3000, distance_nm: 9.1, bearing_deg: 270 }
  const military = { kind: 'military', hex: 'ae0001', callsign: 'RCH123', type: 'C17', model: null, squawk: null, what: null, alt_ft: 9000, distance_nm: 12, bearing_deg: 90 }
  skydSays =
    '@aircraft ' + JSON.stringify([...AIRCRAFT, MAYDAY]) + '\n@alert ' + JSON.stringify(alert) + '\n@alert ' + JSON.stringify(military) + '\n'
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await world.clock.settle()
  skydSays = ''
  // 23:30 is quiet hours: the emergency toasts anyway, the military one doesn't
  expect(toasts).toEqual(['⚠ N7700X · Cessna 172 squawking 7700 (emergency) · 9.1 nm W at 3,000 ft'])
  const buttons = (await ui.findAll({ type: 'Button' })).filter((b: any) => String(b.key).startsWith('pick-'))
  expect(buttons[0].text).toMatch(/^⚠ 7700 N7700X/)
})

test('follow keeps the picked aircraft centred until the map is dragged', async ($, on) => {
  const { ui, controls } = await openWithMap($, on)
  await ui.press({ key: 'pick-a39fe4' })
  await ui.press({ key: 'follow' })
  expect(controls.at(-1)).toContain('follow 1')
  expect((await ui.find({ key: 'follow' }))?.text).toContain('stop following')
  // Dragging takes hold of the map
  await ui.pointer({ type: 'down', button: 'left', x: 50, y: 14, in: 'input' })
  await ui.pointer({ type: 'move', button: 'left', x: 60, y: 14, in: 'input' })
  await ui.pointer({ type: 'up', button: 'left', x: 60, y: 14, in: 'input' })
  expect(controls.at(-1)).toContain('follow 0')
})

test('a swaps altitude colors for status colors', async ($, on) => {
  const { ui, controls } = await openWithMap($, on)
  expect(controls.at(-1)).toContain('colors altitude')
  await ui.press({ key: 'colors' })
  expect(controls.at(-1)).toContain('colors status')
  expect((await ui.find({ key: 'colors' }))?.text).toContain('altitude colors')
})

test('the detail line says how far there is to go and when it lands', async ($, on) => {
  const world = lifecycleWorld($, on)
  const routed = {
    ...AIRCRAFT[1],
    route: { from: { code: 'PIT', city: 'Pittsburgh', lat: 40.49, lon: -80.23 }, to: { code: 'BOS', city: 'Boston', lat: 42.36, lon: -71.01 }, airline: null },
    remaining_nm: 12.4,
    eta_min: 3,
  }
  skydSays = '@aircraft ' + JSON.stringify([routed]) + '\n'
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await world.clock.settle()
  skydSays = ''
  await ui.press({ key: 'pick-a39fe4' })
  expect((await ui.find({ type: 'Text', text: /Pittsburgh → Boston/ }))?.text).toContain('Pittsburgh → Boston · 12 nm to go, ~3 min')
})

test('a narrow pane wraps the keys and keeps the hint on a line of its own', async ($, on) => {
  const world = lifecycleWorld($, on)
  skydSays = '@aircraft ' + JSON.stringify(AIRCRAFT) + '\n'
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 40, placement: 'dock' } as never,
  })
  await world.clock.settle()
  skydSays = ''
  const hint = await ui.find({ type: 'Text', text: /^esc: back to the prompt/ })
  expect(hint).toBeDefined()
  // The hint is its own line, not squeezed in beside the buttons
  expect((await ui.find({ key: 'in' }))?.text).toBe('zoom in')
})

test('a picked flight links out to Flightradar24, FlightAware and adsb.lol, and o opens one', async ($, on) => {
  const opened: string[] = []
  on('process.run', async (_$: unknown, e: { argv: string[] }) => {
    if (e.argv[0] === 'uname') return { value: { exitCode: 0, stdout: 'Darwin\n', stderr: '' } }
    opened.push(e.argv.join(' '))
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  const { ui } = await openWithMap($, on)
  await ui.press({ key: 'pick-a39fe4' })
  const links = (await ui.findAll({ type: 'Link' })).map((l: any) => l.text)
  expect(links).toEqual(['Flightradar24', 'FlightAware', 'adsb.lol'])
  await ui.press({ key: 'open' })
  expect(opened).toEqual(['open https://www.flightradar24.com/JBU1786'])
})

test('the Desktop radar draws the map, the picked track and the altitude scale', async ($, on) => {
  const world = lifecycleWorld($, on)
  const outline = { coast: [[[1, 1], [2, 3], [4, 4]]], runways: [[1, 0, 2, 0]], airports: [['BOS', 1.5, 0]] }
  skydSays =
    '@aircraft ' + JSON.stringify(AIRCRAFT) + '\n@outline ' + JSON.stringify(outline) + '\n@trace ' + JSON.stringify([[-5, 5, 9000], [-4, 4, 8500]]) + '\n'
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'desktop',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await world.clock.settle()
  await ui.press({ key: 'pick-a39fe4' })
  skydSays = ''
  expect(await ui.find({ type: 'Svg' })).toBeDefined()
  expect((await ui.findAll({ type: 'Link' })).length).toBe(3)
})

test('an aircraft spotters tagged gets a toast, a ★ in the list, and a place on the life list', async ($, on) => {
  const toasts: string[] = []
  const world = lifecycleWorld($, on, { isAuto: true, home: SOMERVILLE, isAlerting: true })
  await world.clock.set(new Date(2026, 9, 2, 14, 0).getTime())
  on('ui.toast', async (_$: unknown, e: { text: string }) => {
    toasts.push(e.text)
    return { value: undefined }
  })
  const interest = { group: 'police', category: 'Police Forces', operator: 'Massachusetts State Police', note: 'Patrol' }
  const trooper = { ...AIRCRAFT[1], hex: 'ae1234', callsign: 'MSP1', type: 'B407', interest }
  const alert = { kind: 'interesting', hex: 'ae1234', callsign: 'MSP1', type: 'B407', model: 'Bell 407', squawk: null, what: null, interest, alt_ft: 1200, distance_nm: 2.5, bearing_deg: 180 }
  skydSays = '@aircraft ' + JSON.stringify([trooper]) + '\n@alert ' + JSON.stringify(alert) + '\n'
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 120, placement: 'dock' } as never,
  })
  await world.clock.settle()
  skydSays = ''
  expect(toasts).toEqual(['★ Police: MSP1 · Bell 407 (Massachusetts State Police) · 2.5 nm S at 1,200 ft'])
  expect((await ui.find({ key: 'pick-ae1234' }))?.text).toContain('★ police')
  await ui.press({ key: 'pick-ae1234' })
  expect((await ui.find({ type: 'Text', text: /★ Police Forces/ }))?.text).toContain('★ Police Forces: Massachusetts State Police, Patrol')
  const list = await $.command.run({ command: 'radar', args: 'list' })
  expect(list.text).toContain('★ 1 interesting aircraft: 1 police.')
})

test('a long turn ends with a line about what flew over while Claude worked', async ($, on) => {
  const { clock } = lifecycleWorld($, on)
  // A turn.complete answer the engine would print, which the line goes under
  await $.session.start({ cwd: '/tmp' } as never)
  skydSays = '@aircraft ' + JSON.stringify([...AIRCRAFT, { ...AIRCRAFT[1], hex: 'b00002', callsign: 'DAL977', type: 'A321', distance_nm: 0.8, alt_ft: 1500 }]) + '\n'
  await $.command.run({ command: 'radar', args: '' })
  await $.turn.start({ text: 'refactor it', turnId: 't1' } as never)
  await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await clock.settle()
  skydSays = ''
  await clock.advance(4 * 60 * 1000)
  const result = await $.turn.complete({ reason: 'answer', answer: 'done', durationMs: 240000, isAborted: false, turnId: 't1' } as never)
  expect(result.text).toBe('✈ While Claude worked (4 min): 2 aircraft · closest DAL977 A321, 0.8 nm at 1,500 ft')
})

test('a short turn gets no line', async ($, on) => {
  const { clock } = lifecycleWorld($, on)
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  await $.turn.start({ text: 'hi', turnId: 't1' } as never)
  await clock.advance(10 * 1000)
  const result = await $.turn.complete({ reason: 'answer', answer: 'hello', durationMs: 10000, isAborted: false, turnId: 't1' } as never)
  expect(result.text).toBe('hello')
})

test('b rewinds a minute at a time, as far as skyd keeps, and l goes live', async ($, on) => {
  const world = lifecycleWorld($, on)
  skydSays = '@cells 100 28 ' + blankCells(100, 28) + '\n@history 150\n@aircraft ' + JSON.stringify(AIRCRAFT) + '\n'
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await world.clock.settle()
  skydSays = ''
  await ui.press({ key: 'back' })
  await ui.press({ key: 'back' })
  expect(world.controls.at(-1)).toContain('rewind 120')
  // 150 s of history: no third minute back
  expect(await ui.find({ key: 'back' })).toBeUndefined()
  expect((await ui.find({ type: 'Text', text: /⏪ 2:00 ago/ }))).toBeDefined()
  await ui.press({ key: 'live' })
  expect(world.controls.at(-1)).toContain('rewind 0')
  expect((await $.command.run({ command: 'radar', args: 'rewind 1m' })).text).toBe('Showing the sky 1:00 ago.')
})

test('/radar watch turns a sentence into a rule, and a match toasts once', async ($, on) => {
  const toasts: string[] = []
  const prompts: string[] = []
  on('model.complete', async (_$: unknown, e: { prompt: string }) => {
    prompts.push(e.prompt)
    return { value: { isAnswered: true, text: '```json\n{"label": "Airbus A321s nearby", "model_contains": ["A321"], "max_distance_nm": 10}\n```' } }
  })
  const world = lifecycleWorld($, on, { isAuto: true, home: SOMERVILLE, isAlerting: true })
  await world.clock.set(new Date(2026, 9, 2, 14, 0).getTime())
  on('ui.toast', async (_$: unknown, e: { text: string }) => {
    toasts.push(e.text)
    return { value: undefined }
  })
  const neo = { ...AIRCRAFT[1], model: 'Airbus A321-271NX' }
  skydSays = '@aircraft ' + JSON.stringify([...AIRCRAFT.slice(0, 1), neo]) + '\n@aircraft ' + JSON.stringify([...AIRCRAFT.slice(0, 1), neo]) + '\n'
  await $.session.start({ cwd: '/tmp' } as never)
  const set = await $.command.run({ command: 'radar', args: 'watch any a321 within 10 miles' })
  expect(prompts).toEqual(['any a321 within 10 miles'])
  expect(set.text).toBe('Watching for Airbus A321s nearby (model A321 · within 10 nm). A toast when one shows up.')
  await $.command.run({ command: 'radar', args: '' })
  await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await world.clock.settle()
  skydSays = ''
  // Two polls, one toast
  expect(toasts).toEqual(['👁 Airbus A321s nearby: JBU1786 · Airbus A321-271NX · 6.2 nm NW at 8,375 ft'])
  expect((await $.command.run({ command: 'radar', args: 'watch' })).text).toContain('1. Airbus A321s nearby (model A321 · within 10 nm)')
  expect((await $.command.run({ command: 'radar', args: 'unwatch 1' })).text).toBe('No watches left.')
})

test('Claude can set a watch through its tool, and a request that is no watch is refused', async ($, on) => {
  on('model.complete', async () => ({ value: { isAnswered: true, text: '{"error": "that is about the weather, not aircraft"}' } }))
  lifecycleWorld($, on)
  await $.session.start({ cwd: '/tmp' } as never)
  const answer = await $.tool.call({ tool: 'mcp__overhead__overhead_watch', label: 'Military overhead', groups: ['military'], max_distance_nm: 5 } as never)
  expect(String((answer as { result?: unknown }).result)).toContain('Watching for Military overhead (military · within 5 nm)')
  const refused = await $.command.run({ command: 'radar', args: 'watch will it rain' })
  expect(refused.text).toBe("Couldn't set that watch: that is about the weather, not aircraft")
})

test('the airport strip shows the weather and the runways in use, and Claude hears it too', async ($, on) => {
  const world = lifecycleWorld($, on)
  const ops = {
    airport: 'BOS',
    icao: 'KBOS',
    landing: ['4R', '4L'],
    departing: ['9'],
    metar: { icao: 'KBOS', name: 'Boston/Logan Intl', category: 'MVFR', wind_dir: 40, wind_kt: 12, gust_kt: 20, visibility: '4', ceiling_ft: 1800, temp_c: 12, raw: 'METAR KBOS 030254Z 04012G20KT 4SM BKN018' },
  }
  skydSays = '@aircraft ' + JSON.stringify(AIRCRAFT) + '\n@ops ' + JSON.stringify(ops) + '\n'
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 120, placement: 'dock' } as never,
  })
  await world.clock.settle()
  expect((await ui.find({ type: 'Text', text: 'MVFR' }))).toBeDefined()
  expect((await ui.find({ type: 'Text', text: /wind 040° 12G20 kt/ }))?.text).toBe(
    '· wind 040° 12G20 kt · ceiling 1,800 ft · visibility 4 sm · landing 4R 4L · departing 9',
  )
  const answer = await $.tool.call({ tool: 'mcp__overhead__overhead_now' } as never)
  skydSays = ''
  expect(String((answer as { result?: unknown }).result)).toContain('Nearest busy airport: BOS · MVFR · wind 040° 12G20 kt')
})

// A photo message as skyd sends it, its cells laid out at `columns`
function photoLine(columns: number) {
  const rows = Math.round((columns * 280) / 420 / 2)
  return (
    '@photo ' +
    JSON.stringify({
      hex: 'a39fe4',
      credit: 'Photo: Ethan Yang / Planespotters.net',
      link: 'https://www.planespotters.net/photo/1835120',
      file: '/tmp/a39fe4.rgb',
      width: 420,
      height: 280,
      cells: { columns, rows, data: blankCells(columns, rows) },
      jpeg: '/9j/AAAA',
      jpeg_width: 320,
      jpeg_height: 213,
    }) +
    '\n'
  )
}

async function pickWithPhoto($: any, on: any, surface: 'terminal' | 'desktop', term: string) {
  mock.store(on, { isAuto: true, home: SOMERVILLE })
  const clock = mock.clock(on, { now: 1000 })
  mock.env(on, { TERM_PROGRAM: term })
  on('fs.exists', async () => ({ value: true }))
  on('fs.write', async () => ({ value: undefined }))
  on('session.surfaces', async () => ({ value: ['terminal'] }))
  on('ui.status', async () => ({ value: undefined }))
  on('ui.log', async () => ({ value: undefined }))
  on('ui.open', async () => ({ value: { isPlaced: true } }))
  on('command.register', async () => ({ value: undefined }))
  on('tool.register', async () => ({ value: { tool: 'x' } }))
  on('session.start', async (_$: unknown, e: { cwd: string }) => ({ cwd: e.cwd }))
  let release = () => {}
  on('process.spawn', async function* () {
    // Two answers: the list, then (once it's picked) the photo
    yield { stream: 'stdout' as const, text: '@aircraft ' + JSON.stringify(AIRCRAFT) + '\n' }
    await new Promise<void>((resolve) => (release = resolve))
    yield { stream: 'stdout' as const, text: photoLine(48) }
    await new Promise(() => {})
  })
  await $.session.start({ cwd: '/tmp' } as never)
  await $.command.run({ command: 'radar', args: '' })
  const ui = await $.ui.mount({
    plugin: 'overhead',
    surface,
    component: 'Pane',
    requestId: 'overhead',
    props: { title: 'overhead', isFocused: true, bodyColumns: 100, placement: 'dock' } as never,
  })
  await clock.settle()
  await ui.press({ key: 'pick-a39fe4' })
  release()
  await clock.settle()
  return { ui }
}

test('a picked aircraft shows its photo as cells in Warp, with the credit linking to it', async ($, on) => {
  const { ui } = await pickWithPhoto($, on, 'terminal', 'WarpTerminal')
  expect(await ui.find({ key: 'photo', type: 'Raster' })).toBeDefined()
  expect(await ui.find({ type: 'Link', text: 'Photo: Ethan Yang / Planespotters.net' })).toBeDefined()
  await ui.press({ key: 'photo' })
  expect(await ui.find({ key: 'photo', type: 'Raster' })).toBeUndefined()
})

test('in Ghostty the photo is pixels from the file skyd wrote', async ($, on) => {
  const { ui } = await pickWithPhoto($, on, 'terminal', 'ghostty')
  expect(await ui.find({ key: 'photo', type: 'Image' })).toBeDefined()
})

test('the Desktop app shows the photo as an embedded picture', async ($, on) => {
  const { ui } = await pickWithPhoto($, on, 'desktop', 'WarpTerminal')
  const svgs = await ui.findAll({ type: 'Svg' })
  expect(svgs.length).toBe(2)
})
