---- MODULE SessionManagerFanout ----
EXTENDS FiniteSets, Naturals

CONSTANTS
    Devices,
    FailBudgetInit,
    RemoveDiscoveryOnPartialExpansion,
    MaxRelayCopies,
    InitialAuthorized,
    DiscoveredAuthorized,
    ReplacementAuthorized

ASSUME Devices # {}
ASSUME FailBudgetInit \subseteq Devices
ASSUME MaxRelayCopies \in Nat \ {0}
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
    relayUp,
    relayBag,
    delivered,
    deliveredWhileRevoked,
    failBudget

vars ==
    <<sent, discovery, discovered, replaced, authorized, revoked, cleanupDone,
      msgQ, sessions, relayUp, relayBag, delivered, deliveredWhileRevoked, failBudget>>

ZeroBag ==
    [d \in Devices |-> 0]

BagAdd(b, d) ==
    [b EXCEPT ![d] = IF @ >= MaxRelayCopies THEN @ ELSE @ + 1]

BagDec(b, d) ==
    [b EXCEPT ![d] = IF @ = 0 THEN 0 ELSE @ - 1]

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
    /\ relayUp = TRUE
    /\ relayBag = ZeroBag
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
        sessions, relayUp, relayBag, delivered, deliveredWhileRevoked, failBudget
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
        relayUp, relayBag,
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
        relayUp, relayBag,
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
        sessions, relayUp, relayBag, delivered, deliveredWhileRevoked
      >>

EstablishSession(d) ==
    /\ d \in authorized
    /\ d \notin revoked
    /\ d \notin sessions
    /\ sessions' = sessions \cup {d}
    /\ UNCHANGED <<
        sent, discovery, discovered, replaced, authorized, revoked, cleanupDone,
        msgQ, relayUp, relayBag, delivered, deliveredWhileRevoked, failBudget
      >>

CleanupRevokedDevice(d) ==
    /\ d \in revoked
    /\ d \notin cleanupDone
    /\ msgQ' = msgQ \ {d}
    /\ sessions' = sessions \ {d}
    /\ relayBag' = [relayBag EXCEPT ![d] = 0]
    /\ cleanupDone' = cleanupDone \cup {d}
    /\ UNCHANGED <<
        sent, discovery, discovered, replaced, authorized, revoked,
        relayUp, delivered, deliveredWhileRevoked, failBudget
      >>

Flush(d) ==
    /\ d \in sessions
    /\ d \in msgQ
    /\ d \in authorized
    /\ d \notin revoked
    /\ relayUp
    /\ relayBag[d] < MaxRelayCopies
    /\ relayBag' = BagAdd(relayBag, d)
    /\ UNCHANGED <<
        sent, discovery, discovered, replaced, authorized, revoked, cleanupDone,
        msgQ, sessions, relayUp, delivered, deliveredWhileRevoked, failBudget
      >>

RelayDeliver(d) ==
    /\ relayUp
    /\ relayBag[d] > 0
    /\ d \in authorized
    /\ d \notin revoked
    /\ relayBag' = BagDec(relayBag, d)
    /\ msgQ' = msgQ \ {d}
    /\ delivered' = delivered \cup {d}
    /\ deliveredWhileRevoked' =
        IF d \in revoked \/ d \in cleanupDone
            THEN deliveredWhileRevoked \cup {d}
            ELSE deliveredWhileRevoked
    /\ UNCHANGED <<
        sent, discovery, discovered, replaced, authorized, revoked, cleanupDone,
        sessions, relayUp, failBudget
      >>

RelayDrop(d) ==
    /\ relayBag[d] > 0
    /\ relayBag' = BagDec(relayBag, d)
    /\ UNCHANGED <<
        sent, discovery, discovered, replaced, authorized, revoked, cleanupDone,
        msgQ, sessions, relayUp, delivered, deliveredWhileRevoked, failBudget
      >>

RelayDuplicate(d) ==
    /\ relayBag[d] > 0
    /\ relayBag[d] < MaxRelayCopies
    /\ relayBag' = BagAdd(relayBag, d)
    /\ UNCHANGED <<
        sent, discovery, discovered, replaced, authorized, revoked, cleanupDone,
        msgQ, sessions, relayUp, delivered, deliveredWhileRevoked, failBudget
      >>

RelayPartition ==
    /\ relayUp
    /\ relayUp' = FALSE
    /\ UNCHANGED <<
        sent, discovery, discovered, replaced, authorized, revoked, cleanupDone,
        msgQ, sessions, relayBag, delivered, deliveredWhileRevoked, failBudget
      >>

RelayRecover ==
    /\ ~relayUp
    /\ relayUp' = TRUE
    /\ UNCHANGED <<
        sent, discovery, discovered, replaced, authorized, revoked, cleanupDone,
        msgQ, sessions, relayBag, delivered, deliveredWhileRevoked, failBudget
      >>

\* Explicit delay step when transport has in-flight packets.
RelayDelay ==
    /\ \E d \in Devices: relayBag[d] > 0
    /\ UNCHANGED vars

Stutter == UNCHANGED vars

RelayEventuallyRecovers ==
    <>[]relayUp

Next ==
    \/ Send
    \/ AppKeysDiscover
    \/ AppKeysRevoke
    \/ ExpandDiscovery
    \/ \E d \in Devices: EstablishSession(d)
    \/ \E d \in Devices: CleanupRevokedDevice(d)
    \/ \E d \in Devices: Flush(d)
    \/ \E d \in Devices: RelayDeliver(d)
    \/ \E d \in Devices: RelayDrop(d)
    \/ \E d \in Devices: RelayDuplicate(d)
    \/ RelayPartition
    \/ RelayRecover
    \/ RelayDelay
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
    /\ \A d \in Devices: SF_vars(RelayDeliver(d))

\* Recovery-conditioned spec for liveness checks.
SpecUnderRecovery ==
    /\ Spec
    /\ RelayEventuallyRecovers

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

\* Cleanup also purges in-flight local transport attempts for revoked devices.
NoRelayInflightForRevokedAfterCleanup ==
    \A d \in cleanupDone: relayBag[d] = 0

\* Liveness goal under weak fairness:
\* if a device remains authorized after send, then it is eventually delivered.
AuthorizedEventuallyDeliveredUnderRecovery ==
    \A d \in Devices:
        [](
            (sent /\ d \in authorized /\ d \notin revoked /\ <>[](d \in authorized /\ d \notin revoked))
                => <>(d \in delivered)
          )

\* Revoked devices should eventually be purged from queue/session/transport state.
RevokedEventuallyPurged ==
    \A d \in Devices:
        [](
            (d \in revoked)
                => <>(d \in cleanupDone /\ d \notin msgQ /\ d \notin sessions /\ relayBag[d] = 0)
          )

====
