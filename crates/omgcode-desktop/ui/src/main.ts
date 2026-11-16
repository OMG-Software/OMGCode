import { el, replaceChildren } from "./dom";
import { hostAvailable, NO_HOST_MESSAGE } from "./ipc/client";
import { startMain } from "./main/app";

const root = document.getElementById("app");
if (root !== null) {
  if (hostAvailable()) {
    startMain(root);
  } else {
    replaceChildren(root, el("p", { class: "no-host", text: NO_HOST_MESSAGE }));
  }
}
