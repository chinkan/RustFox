import { describe, expect, it, vi } from 'vitest'
import { BootWatcher } from './useBootWatcher'
import type { Api } from '../api/client'

function fakeHealthApi(bootId: string, fail = false): Api {
  return {
    getHealth: async () => {
      if (fail) throw new TypeError('fetch failed')
      return {
        cpuPercent: 1,
        memUsedGb: 1,
        memTotalGb: 2,
        diskUsedPercent: 3,
        uptimeHours: 1,
        bootId,
      }
    },
  } as unknown as Api
}

describe('BootWatcher (ADR 0008B)', () => {
  it('marks offline on poll failure without firing restart', async () => {
    const onRestart = vi.fn()
    const w = new BootWatcher(fakeHealthApi('b1', true), onRestart)
    await w.tick()
    expect(w.getSnapshot().online).toBe(false)
    expect(onRestart).not.toHaveBeenCalled()
  })

  it('first successful poll seeds bootId and does NOT count as restart', async () => {
    const onRestart = vi.fn()
    const w = new BootWatcher(fakeHealthApi('b1'), onRestart)
    await w.tick()
    expect(w.getSnapshot()).toMatchObject({ online: true, bootId: 'b1', reconnectEpoch: 0 })
    expect(onRestart).not.toHaveBeenCalled()
  })

  it('changed bootId fires onRestart + bumps epoch', async () => {
    const onRestart = vi.fn()
    const w = new BootWatcher(fakeHealthApi('b1'), onRestart)
    await w.tick()
    // simulate restart: swap the api to a new boot id
    ;(w as unknown as { api: Api }).api = fakeHealthApi('b2')
    await w.tick()
    expect(onRestart).toHaveBeenCalledWith('b2')
    expect(w.getSnapshot().reconnectEpoch).toBe(1)
    expect(w.getSnapshot().online).toBe(true)
  })

  it('recovery with the SAME bootId after offline window still re-auths once', async () => {
    const onRestart = vi.fn()
    let fail = true
    const api = {
      getHealth: async () => {
        if (fail) throw new TypeError('down')
        return { cpuPercent: 0, memUsedGb: 0, memTotalGb: 0, diskUsedPercent: 0, uptimeHours: 0, bootId: 'same' }
      },
    } as unknown as Api
    const w = new BootWatcher(api, onRestart)
    await w.tick() // bootId=null → fail → offline
    expect(w.getSnapshot().online).toBe(false)
    fail = false
    await w.tick() // first success seeds bootId but was offline before → reconcile once
    await w.tick()
    expect(onRestart).toHaveBeenCalledTimes(1)
    expect(w.getSnapshot().online).toBe(true)
  })
})
