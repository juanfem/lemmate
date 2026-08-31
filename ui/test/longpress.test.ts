import { test } from 'node:test'
import assert from 'node:assert/strict'
import { HOLD_MS, SLOP_PX, longPress, type Point } from '../src/lib/longpress.ts'

/** A hand-cranked clock, so the gesture can be run without waiting half a second for it. */
function rig() {
  const held: Point[] = []
  let pending: (() => void) | null = null
  const gesture = longPress({
    onHold: (at) => held.push(at),
    setTimer: (fn) => ((pending = fn), 1),
    clearTimer: () => (pending = null),
  })
  return {
    gesture,
    held,
    get armed() {
      return pending !== null
    },
    /** Let the hold time pass. Does nothing once the timer has been cleared. */
    tick() {
      const fn = pending
      pending = null
      fn?.()
    },
  }
}

test('a finger held still opens the menu where it rests', () => {
  const r = rig()
  r.gesture.down({ x: 40, y: 120 }, 'touch')
  r.gesture.move({ x: 42, y: 123 })
  r.tick()
  assert.deepEqual(r.held, [{ x: 40, y: 120 }])
})

test('a finger that drifts is a scroll, not a press', () => {
  const r = rig()
  r.gesture.down({ x: 40, y: 120 }, 'touch')
  r.gesture.move({ x: 40, y: 120 + SLOP_PX + 1 })
  assert.equal(r.armed, false)
  r.tick()
  assert.deepEqual(r.held, [])
})

test('lifting before the hold is up cancels it', () => {
  const r = rig()
  r.gesture.down({ x: 5, y: 5 }, 'touch')
  r.gesture.end()
  r.tick()
  assert.deepEqual(r.held, [])
})

test('a mouse keeps its own right button and never arms the timer', () => {
  const r = rig()
  r.gesture.down({ x: 5, y: 5 }, 'mouse')
  assert.equal(r.armed, false)
  r.tick()
  assert.deepEqual(r.held, [])
})

test('the click that ends a fired press is swallowed exactly once', () => {
  const r = rig()
  r.gesture.down({ x: 5, y: 5 }, 'touch')
  r.tick()
  r.gesture.end()
  assert.equal(r.gesture.swallowClick(), true, 'the lift must not also open the note')
  assert.equal(r.gesture.swallowClick(), false, 'and only that one click')
})

test('a press that never fired lets its click through', () => {
  const r = rig()
  r.gesture.down({ x: 5, y: 5 }, 'touch')
  r.gesture.end()
  assert.equal(r.gesture.swallowClick(), false)
})

test('a new press disarms a swallow left over from an abandoned one', () => {
  const r = rig()
  r.gesture.down({ x: 5, y: 5 }, 'touch')
  r.tick()
  // The menu opened and was dismissed by something other than a click on this row.
  r.gesture.down({ x: 5, y: 5 }, 'touch')
  r.gesture.end()
  assert.equal(r.gesture.swallowClick(), false)
})

test('the defaults are the ones the stylesheets and comments quote', () => {
  assert.equal(HOLD_MS, 500)
  assert.equal(SLOP_PX, 10)
})
