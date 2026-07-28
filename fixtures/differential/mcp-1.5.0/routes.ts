import { authorize, showUser } from "./handlers";

export function registerRoutes() {
  router.get("/users/:id", authorize, showUser);
}

export function dispatchReady() {
  bus.emit("ready");
}

export function registerReady() {
  bus.on("ready", showUser);
}
