/** Fence requests by selection lifetime, even when a transport ignores cancellation. */
export class SessionRequests {
  private controller = new AbortController()
  private epoch = 0
  loading = false

  capture(): SessionRequest {
    return { epoch: this.epoch, signal: this.controller.signal }
  }

  begin(): SessionRequest {
    this.controller.abort()
    this.controller = new AbortController()
    this.epoch += 1
    this.loading = true
    return this.capture()
  }

  isCurrent(request: SessionRequest): boolean {
    return request.epoch === this.epoch && !request.signal.aborted
  }

  finish(request: SessionRequest): boolean {
    if (!this.isCurrent(request)) return false
    this.loading = false
    return true
  }

  dispose(): void {
    this.controller.abort()
    this.epoch += 1
    this.loading = false
  }
}

export type SessionRequest = { epoch: number; signal: AbortSignal }
