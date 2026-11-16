// Holds the picture that the bash-screen mod shows. The app reloads a picture saved
// under mods/, and reloads the mod whose folder it is in; this folder takes that
// reload, so the bash-screen mod keeps running while its picture changes.
import { View, div } from "gpui-kit";

export default class Frame extends View {
  render(_cx) {
    return div();
  }
}
