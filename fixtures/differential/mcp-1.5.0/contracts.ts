export interface Handler {
  handle(): void;
}

export class ConcreteHandler implements Handler {
  handle() {}
}

export function dispatch(handler: Handler) {
  handler.handle();
}
