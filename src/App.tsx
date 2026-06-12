import { Toolbar } from "./components/Toolbar";
import { ViewerArea } from "./components/ViewerArea";

function App() {
  return (
    <div className="app-shell">
      <Toolbar />
      <div className="viewer-area">
        <ViewerArea />
      </div>
    </div>
  );
}

export default App;
