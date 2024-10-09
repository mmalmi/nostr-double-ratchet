import React, { useState, useEffect, useMemo } from "react";
import { BrowserRouter as Router, Route, Routes, useParams, useNavigate, Link } from "react-router-dom";
import SearchBox from "./ui/SearchBox";
import ProfileCard from "./cards/ProfileCard";
import useLocalStorage from "./hooks/useLocalStorage";
import { socialGraphLoaded, LOCALSTORAGE_PUBLICKEY, DEFAULT_SOCIAL_GRAPH_ROOT } from "./utils/socialGraph";
import Explore from "./cards/Explore";
import { Avatar } from "./ui/Avatar";
import { nip19 } from "nostr-tools";
import Stats from "./cards/Stats";

const Main = () => {
    const { selectedUser: paramSelectedUser } = useParams();
    const navigate = useNavigate();
    const [viewAs, setViewAs] = useLocalStorage(LOCALSTORAGE_PUBLICKEY, DEFAULT_SOCIAL_GRAPH_ROOT);
    const [ready, setReady] = useState(false);

    const selectedUser = useMemo(() => paramSelectedUser && nip19.decode(paramSelectedUser).data)

    useEffect(() => {
        socialGraphLoaded.then(() => setReady(true));
    }, []);

    const onSelect = (pubkey) => {
        navigate(`/${nip19.npubEncode(pubkey)}`);
    };

    const viewAsSelectedUser = () => {
        setViewAs(selectedUser)
    }

    if (!ready) {
        return null;
    }

    return (
        <div className="flex flex-col gap-8 p-4 w-full max-w-prose">
            <div className="flex flex-row justify-between flex-wrap items-center">
                <h1 className="text-2xl">Nostr Social Graph</h1>
                <Link to={`/${nip19.npubEncode(viewAs)}`} className="cursor-pointer">
                    <Avatar pubKey={viewAs} />
                </Link>
            </div>
            <SearchBox onSelect={onSelect} />
            <ProfileCard pubKey={selectedUser ?? viewAs} currentUser={viewAs} viewAsSelectedUser={viewAsSelectedUser} />
            <Explore pubKey={viewAs} selectedUser={selectedUser} />
            <Stats />
            <div className="text-sm text-gray-500 mt-4 flex flex-row gap-4">
                <a href="https://github.com/mmalmi/nostr-social-graph" target="_blank" rel="noopener noreferrer" className="link">
                    GitHub
                </a>
                <a href="https://www.npmjs.com/package/nostr-social-graph" target="_blank" rel="noopener noreferrer" className="link">
                    npm
                </a>
            </div>
        </div>
    );
};

export const App = () => (
    <Router>
        <Routes>
            <Route path="/:selectedUser?" element={<Main />} />
        </Routes>
    </Router>
);