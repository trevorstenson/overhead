// Laid over the radar picture. Positions are posted as fractions of the
// picture's width and height, so they mean the same at any size:
//   { click, x, y }      a left click that didn't move: skyd picks the
//                        aircraft drawn nearest
//   { double, x, y }     a second click soon after and near the first: zoom
//                        in there
//   { drag: [dx, dy] }   the left button held and moved, since the last post
// Keys are left alone, so the pane's hotkeys keep working.

// A press that moves less than this, in cells, is a click, not a drag
const DRAG_THRESHOLD = 0.6
const DOUBLE_MS = 400
// How near the first click a second must land to make a double, in cells
const DOUBLE_REACH = 2

export default function RadarInput(props, surface) {
  if (surface.state === undefined) {
    const columns = () => Math.max(1, props.columns)
    const rows = () => Math.max(1, props.rows)
    // The sub-cell position where the terminal reports one, else mid-cell
    const at = (e) => ({ x: e.fine?.x ?? e.x + 0.5, y: e.fine?.y ?? e.y + 0.5 })
    let press = null // { start, last, isDrag }
    let pending = { x: 0, y: 0 }
    let lastClick = null // { x, y, time }
    let clicks = 0

    surface.onPointer((e) => {
      if (e.button !== 'left' && e.type !== 'move') return
      const p = at(e)
      if (e.type === 'down') {
        press = { start: p, last: p, isDrag: false }
        pending = { x: 0, y: 0 }
        return
      }
      if (!press) return
      if (e.type === 'move') {
        pending.x += p.x - press.last.x
        pending.y += p.y - press.last.y
        press.last = p
        // Cells are about twice as tall as wide
        if (Math.hypot(p.x - press.start.x, (p.y - press.start.y) * 2) > DRAG_THRESHOLD) press.isDrag = true
        return
      }
      if (e.type === 'up') {
        const wasDrag = press.isDrag
        press = null
        if (wasDrag) {
          // What moved since the last post
          if (pending.x || pending.y) surface.post({ drag: [pending.x / columns(), pending.y / rows()] })
          pending = { x: 0, y: 0 }
          return
        }
        const now = Date.now()
        const isDouble =
          lastClick && now - lastClick.time < DOUBLE_MS && Math.hypot(p.x - lastClick.x, p.y - lastClick.y) < DOUBLE_REACH
        lastClick = isDouble ? null : { ...p, time: now }
        clicks += 1
        surface.post(isDouble ? { double: clicks, x: p.x / columns(), y: p.y / rows() } : { click: clicks, x: p.x / columns(), y: p.y / rows() })
      }
    })

    // Drags go out at most every 33 ms, gathered, so the map follows the
    // pointer without a post for every cell crossed
    surface.every(33, () => {
      if (!press?.isDrag || (pending.x === 0 && pending.y === 0)) return
      surface.post({ drag: [pending.x / columns(), pending.y / rows()] })
      pending = { x: 0, y: 0 }
    })

    surface.setState({})
  }
  const { Box } = surface.elements
  return Box({ width: '100%', height: '100%' })
}
