import React from "react";
import { Avatar } from "./Avatar";
import { Name } from "./Name";

interface CurrentUserProps {
  pubKey: string;
  setSelectedUser: (pubKey: string) => void;
}

const CurrentUser: React.FC<CurrentUserProps> = ({ pubKey, setSelectedUser }) => {
  return (
    <div className="flex flex-col gap-4 cursor-pointer" onClick={() => setSelectedUser(pubKey)}>
      <div className="flex flex-row gap-4 items-center">
        <Avatar pubKey={pubKey} />
        <div className="flex flex-col">
          <Name pubKey={pubKey} />
          <small>Current user</small>
        </div>
      </div>
    </div>
  );
};

export default CurrentUser;
