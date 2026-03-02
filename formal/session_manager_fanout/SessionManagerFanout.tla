---- MODULE SessionManagerFanout ----
EXTENDS FiniteSets

CONSTANTS
    Devices,
    FailBudgetInit,
    RemoveDiscoveryOnPartialExpansion,
    InitialAuthorized,
    DiscoveredAuthorized,
    ReplacementAuthorized

ASSUME Devices # {}
ASSUME FailBudgetInit \subseteq Devices
ASSUME InitialAuthorized \subseteq Devices
ASSUME DiscoveredAuthorized \subseteq Devices
ASSUME ReplacementAuthorized \subseteq Devices

VARIABLES
    sent,
    discovery,
    discovered,
    replaced,
    authorized,
    revoked,
    cleanupDone,
    msgQ,
    sessions,
    delivered,
    deliveredWhileRevoked,
    failBudget

vars ==
    <<sent, discovery, discovered, replaced, authorized, revoked, cleanupDone,
      msgQ, sessions, delivered, deliveredWhileRevoked, failBudget>>

Init ==
    /\ sent = FALSE
    /\ discovery = FALSE
    /\ discovered = FALSE
    /\ replaced = FALSE
    /\ authorized = InitialAuthorized
    /\ revoked = {}
    /\ cleanupDone = {}
    /\ msgQ = {}
    /\ sessions = {}
    /\ delivered = {}
    /\ deliveredWhileRevoked = {}
    /\ failBudget = FailBudgetInit

Send ==
    /\ ~sent
    /\ sent' = TRUE
    /\ IF authorized = {}
          THEN /\ discovery' = TRUE
               /\ msgQ' = msgQ
          ELSE /\ discovery' = discovery
               /\ msgQ' = msgQ \cup authorized
    /\ UNCHANGED <<
        discovered, replaced, authorized, revoked, cleanupDone,
        sessions, delivered, deliveredWhileRevoked, failBudget
      >>

ApplyAppKeys(newAuthorized) ==
    LET removed == authorized \ newAuthorized IN
        /\ authorized' = newAuthorized
        /\ revoked' = (revoked \cup removed) \ newAuthorized
        /\ cleanupDone' = cleanupDone \ newAuthorized

AppKeysDiscover ==
    /\ ~discovered
    /\ discovered' = TRUE
    /\ ApplyAppKeys(DiscoveredAuthorized)
    /\ UNCHANGED <<
        sent, discovery, replaced, msgQ, sessions,
        delivered, deliveredWhileRevoked, failBudget
      >>

AppKeysRevoke ==
    /\ discovered
    /\ ~replaced
    /\ ReplacementAuthorized # authorized
    /\ replaced' = TRUE
    /\ ApplyAppKeys(ReplacementAuthorized)
    /\ UNCHANGED <<
        sent, discovery, discovered, msgQ, sessions,
        delivered, deliveredWhileRevoked, failBudget
      >>

ExpandDiscovery ==
    /\ discovery
    /\ authorized # {}
    /\ \E fail \in SUBSET (failBudget \cap authorized):
        LET succ == authorized \ fail IN
            /\ msgQ' = msgQ \cup succ
            /\ failBudget' = failBudget \ fail
            /\ discovery' =
                IF RemoveDiscoveryOnPartialExpansion
                    THEN FALSE
                    ELSE fail # {}
    /\ UNCHANGED <<
        sent, discovered, replaced, authorized, revoked, cleanupDone,
        sessions, delivered, deliveredWhileRevoked
      >>

EstablishSession(d) ==
    /\ d \in authorized
    /\ d \notin revoked
    /\ d \notin sessions
    /\ sessions' = sessions \cup {d}
    /\ UNCHANGED <<
        sent, discovery, discovered, replaced, authorized, revoked, cleanupDone,
        msgQ, delivered, deliveredWhileRevoked, failBudget
      >>

CleanupRevokedDevice(d) ==
    /\ d \in revoked
    /\ d \notin cleanupDone
    /\ msgQ' = msgQ \ {d}
    /\ sessions' = sessions \ {d}
    /\ cleanupDone' = cleanupDone \cup {d}
    /\ UNCHANGED <<
        sent, discovery, discovered, replaced, authorized, revoked,
        delivered, deliveredWhileRevoked, failBudget
      >>

Flush(d) ==
    /\ d \in sessions
    /\ d \in msgQ
    /\ d \in authorized
    /\ d \notin revoked
    /\ msgQ' = msgQ \ {d}
    /\ delivered' = delivered \cup {d}
    /\ deliveredWhileRevoked' =
        IF d \in revoked \/ d \in cleanupDone
            THEN deliveredWhileRevoked \cup {d}
            ELSE deliveredWhileRevoked
    /\ UNCHANGED <<
        sent, discovery, discovered, replaced, authorized, revoked, cleanupDone,
        sessions, failBudget
      >>

Stutter == UNCHANGED vars

Next ==
    \/ Send
    \/ AppKeysDiscover
    \/ AppKeysRevoke
    \/ ExpandDiscovery
    \/ \E d \in Devices: EstablishSession(d)
    \/ \E d \in Devices: CleanupRevokedDevice(d)
    \/ \E d \in Devices: Flush(d)
    \/ Stutter

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(Send)
    /\ WF_vars(AppKeysDiscover)
    /\ WF_vars(AppKeysRevoke)
    /\ WF_vars(ExpandDiscovery)
    /\ \A d \in Devices: WF_vars(CleanupRevokedDevice(d))
    /\ \A d \in Devices: WF_vars(EstablishSession(d))
    /\ \A d \in Devices: WF_vars(Flush(d))

\* Once we have sent, every currently authorized device must keep the message represented
\* somewhere: either still in discovery, queued per-device, or delivered.
NoDropAuthorized ==
    \A d \in authorized:
        ~sent \/ discovery \/ d \in msgQ \/ d \in delivered

\* No delivery to revoked devices once revocation/cleanup logic is in play.
NoDeliverToRevokedAfterCleanup ==
    deliveredWhileRevoked = {}

\* Cleanup must purge queue entries for explicitly cleaned revoked devices.
NoQueueForRevokedAfterCleanup ==
    \A d \in cleanupDone: d \notin msgQ

\* Liveness goal under weak fairness:
\* if a device remains authorized after send, then it is eventually delivered.
AuthorizedEventuallyDeliveredUnderRecovery ==
    \A d \in Devices:
        [](
            (sent /\ d \in authorized /\ d \notin revoked /\ <>[](d \in authorized /\ d \notin revoked))
                => <>(d \in delivered)
          )

\* Revoked devices should eventually be purged from queue/session state.
RevokedEventuallyPurged ==
    \A d \in Devices:
        []((d \in revoked) => <>(d \in cleanupDone /\ d \notin msgQ /\ d \notin sessions))

====
