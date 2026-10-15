/**
 * Home (re-skin v2): the customizable prototype Home lives in `features/home` (layout model,
 * widgets, customize panels, theme). This route module only wires it into the router.
 */
import Home from "../features/home/Home";

export { dueBadge, HOME_TODO_LIMIT } from "../features/home/widgets";

export function meta() {
  return [{ title: "Home · SutraERP" }];
}

export default Home;
