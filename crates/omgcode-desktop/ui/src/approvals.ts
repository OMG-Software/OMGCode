import { startApprovals } from "./approvals/app";
import { el, replaceChildren } from "./dom";
import { hostAvailable, NO_HOST_MESSAGE } from "./ipc/client";

const root = document.getElementById("app");
if (root !== null) {
  if (hostAvailable()) {
    startApprovals(root);
  } else {
    replaceChildren(root, el("p", { class: "no-host", text: NO_HOST_MESSAGE }));
  }
}
