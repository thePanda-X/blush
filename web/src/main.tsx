import { createRoot } from "react-dom/client";
import { App } from "./components/App";
import { connect } from "./lib/store";

connect();
createRoot(document.getElementById("root")!).render(<App />);
