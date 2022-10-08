import rspc from "./query.tauri";

function App() {
  const { data } = rspc.useQuery(["version"]);

  return (
    <div className="App">
      <h1>You are running v{data}</h1>
      <h3>This data is from the rust side</h3>
    </div>
  );
}

export default App;
