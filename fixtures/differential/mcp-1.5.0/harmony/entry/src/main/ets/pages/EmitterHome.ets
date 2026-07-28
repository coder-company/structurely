import { emitter as bus } from '@kit.BasicServicesKit'

function receiveRefresh() {
  return "received"
}

export function registerRefresh() {
  const refresh = { eventId: 7 }
  bus.on(refresh, receiveRefresh)
}

export function sendRefresh() {
  bus.emit({ eventId: 7 })
}
