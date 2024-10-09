import React, { useState, useEffect } from "react";
import socialGraph, {loadFromFile, saveToFile, loadAndMerge} from "../utils/socialGraph";

const Stats = () => {
  const [graphData, setGraphData] = useState({
    users: 0,
    follows: 0,
    sizeByDistance: {}
  });

  useEffect(() => {
    const updateGraphData = () => {
      setGraphData(socialGraph().size());
    };

    updateGraphData(); // Initial call
    const intervalId = setInterval(updateGraphData, 1000); // Update every second

    return () => clearInterval(intervalId); // Cleanup on unmount
  }, []);

  return (
    <div className="flex flex-col gap-4 p-4 rounded-xl bg-base-100">
      <div className="flex flex-row flex-wrap justify-between items-start gap-4 uppercase text-xs text-base-content/50 font-bold">
        Stored Data
      </div>
      <div><strong>Users:</strong> {graphData.users}</div>
      <div><strong>Follow relationships:</strong> {graphData.follows}</div>
      <div className="flex flex-row gap-4">
        <button className="btn btn-neutral btn-sm rounded-full" onClick={() => saveToFile()}>Save to file</button>
        <button className="btn btn-neutral btn-sm rounded-full" onClick={() => loadFromFile()}>Load from file</button>
        <button className="btn btn-neutral btn-sm rounded-full" onClick={() => loadAndMerge()}>Load & merge</button>
      </div>
    </div>
  );
};

export default Stats;