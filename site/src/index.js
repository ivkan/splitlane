// The Worker on splitlane.dev; what it does is in site.js.
//
// This module exports the handler and nothing else. With static assets in
// front, the runtime takes every named export of the main module for an
// entrypoint and refuses to start on one that is a string, such as the
// policy or the page's address.
import { handle } from "./site.js";

export default {
  fetch: handle,
};
