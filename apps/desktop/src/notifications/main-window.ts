// What the main window starts besides the store: opening items from the menu
// bar panel and notifications, and following the language setting.
import { followLanguageSetting } from "../views/settings/language";
import { startNavigateListener } from "./navigate";

startNavigateListener();
followLanguageSetting();
