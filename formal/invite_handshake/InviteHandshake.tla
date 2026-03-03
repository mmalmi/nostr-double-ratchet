---- MODULE InviteHandshake ----
EXTENDS FiniteSets, Naturals

CONSTANTS
    ResponseIds,
    MaxRelayCopies,
    BugAllowUnauthorizedClaim,
    BugAllowReplaySession,
    SelfResponseIds,
    ValidResponseIds,
    TargetedResponseIds,
    KnownAppKeysResponseIds,
    AppKeysAuthorizedResponseIds,
    CachedAuthorizedResponseIds,
    SingleDeviceResponseIds

ASSUME ResponseIds # {}
ASSUME MaxRelayCopies \in Nat \ {0}
ASSUME SelfResponseIds \subseteq ResponseIds
ASSUME ValidResponseIds \subseteq ResponseIds
ASSUME TargetedResponseIds \subseteq ResponseIds
ASSUME KnownAppKeysResponseIds \subseteq ResponseIds
ASSUME AppKeysAuthorizedResponseIds \subseteq ResponseIds
ASSUME CachedAuthorizedResponseIds \subseteq ResponseIds
ASSUME SingleDeviceResponseIds \subseteq ResponseIds

VARIABLES
    relayUp,
    relayBag,
    inbox,
    processed,
    replayed,
    acceptedResponses,
    rejectedResponses,
    unauthorizedAccepted,
    sessionCreations

vars ==
    <<relayUp, relayBag, inbox, processed, replayed,
      acceptedResponses, rejectedResponses, unauthorizedAccepted,
      sessionCreations>>

ZeroBag ==
    [id \in ResponseIds |-> 0]

ZeroCreations ==
    [id \in ResponseIds |-> 0]

BagAdd(b, id) ==
    [b EXCEPT ![id] = IF @ >= MaxRelayCopies THEN @ ELSE @ + 1]

BagDec(b, id) ==
    [b EXCEPT ![id] = IF @ = 0 THEN 0 ELSE @ - 1]

ResponsePassesEnvelope(id) ==
    /\ id \in ValidResponseIds
    /\ id \in TargetedResponseIds
    /\ id \notin SelfResponseIds

IsAuthorized(id) ==
    IF id \in KnownAppKeysResponseIds
        THEN id \in AppKeysAuthorizedResponseIds
        ELSE id \in CachedAuthorizedResponseIds \/ id \in SingleDeviceResponseIds

IsAcceptable(id) ==
    /\ ResponsePassesEnvelope(id)
    /\ IsAuthorized(id)

Init ==
    /\ relayUp = TRUE
    /\ relayBag = ZeroBag
    /\ inbox = {}
    /\ processed = {}
    /\ replayed = {}
    /\ acceptedResponses = {}
    /\ rejectedResponses = {}
    /\ unauthorizedAccepted = {}
    /\ sessionCreations = ZeroCreations

Emit(id) ==
    /\ id \notin acceptedResponses
    /\ id \notin rejectedResponses
    /\ id \notin inbox
    /\ relayBag[id] = 0
    /\ relayBag' = BagAdd(relayBag, id)
    /\ UNCHANGED <<
        relayUp, inbox, processed, replayed,
        acceptedResponses, rejectedResponses, unauthorizedAccepted,
        sessionCreations
      >>

RelayDeliver(id) ==
    /\ relayUp
    /\ relayBag[id] > 0
    /\ relayBag' = BagDec(relayBag, id)
    /\ inbox' = inbox \cup {id}
    /\ replayed' =
        IF id \in inbox \/ id \in processed
            THEN replayed \cup {id}
            ELSE replayed
    /\ UNCHANGED <<
        relayUp, processed,
        acceptedResponses, rejectedResponses, unauthorizedAccepted,
        sessionCreations
      >>

RelayDrop(id) ==
    /\ relayBag[id] > 0
    /\ relayBag' = BagDec(relayBag, id)
    /\ UNCHANGED <<
        relayUp, inbox, processed, replayed,
        acceptedResponses, rejectedResponses, unauthorizedAccepted,
        sessionCreations
      >>

RelayDuplicate(id) ==
    /\ relayBag[id] > 0
    /\ relayBag[id] < MaxRelayCopies
    /\ relayBag' = BagAdd(relayBag, id)
    /\ UNCHANGED <<
        relayUp, inbox, processed, replayed,
        acceptedResponses, rejectedResponses, unauthorizedAccepted,
        sessionCreations
      >>

RelayPartition ==
    /\ relayUp
    /\ relayUp' = FALSE
    /\ UNCHANGED <<
        relayBag, inbox, processed, replayed,
        acceptedResponses, rejectedResponses, unauthorizedAccepted,
        sessionCreations
      >>

RelayRecover ==
    /\ ~relayUp
    /\ relayUp' = TRUE
    /\ UNCHANGED <<
        relayBag, inbox, processed, replayed,
        acceptedResponses, rejectedResponses, unauthorizedAccepted,
        sessionCreations
      >>

RelayDelay ==
    /\ \E id \in ResponseIds: relayBag[id] > 0
    /\ UNCHANGED vars

ProcessResponse(id) ==
    /\ id \in inbox
    /\ LET acceptable == IsAcceptable(id)
           envelopePasses == ResponsePassesEnvelope(id)
           shouldAccept ==
               IF id \in processed
                   THEN acceptable /\ BugAllowReplaySession
                   ELSE acceptable \/ (BugAllowUnauthorizedClaim /\ envelopePasses)
       IN
       /\ inbox' = inbox \ {id}
       /\ processed' = processed \cup {id}
       /\ IF shouldAccept
             THEN /\ acceptedResponses' = acceptedResponses \cup {id}
                  /\ rejectedResponses' = rejectedResponses
                  /\ unauthorizedAccepted' =
                      IF acceptable
                          THEN unauthorizedAccepted
                          ELSE unauthorizedAccepted \cup {id}
                  /\ sessionCreations' = [sessionCreations EXCEPT ![id] = @ + 1]
             ELSE /\ acceptedResponses' = acceptedResponses
                  /\ rejectedResponses' = rejectedResponses \cup {id}
                  /\ unauthorizedAccepted' = unauthorizedAccepted
                  /\ sessionCreations' = sessionCreations
    /\ UNCHANGED <<relayUp, relayBag, replayed>>

Stutter ==
    UNCHANGED vars

RelayEventuallyRecovers ==
    <>[]relayUp

Next ==
    \/ \E id \in ResponseIds: Emit(id)
    \/ \E id \in ResponseIds: RelayDeliver(id)
    \/ \E id \in ResponseIds: RelayDrop(id)
    \/ \E id \in ResponseIds: RelayDuplicate(id)
    \/ RelayPartition
    \/ RelayRecover
    \/ RelayDelay
    \/ \E id \in ResponseIds: ProcessResponse(id)
    \/ Stutter

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ \A id \in ResponseIds: WF_vars(Emit(id))
    /\ \A id \in ResponseIds: SF_vars(RelayDeliver(id))
    /\ \A id \in ResponseIds: WF_vars(ProcessResponse(id))

SpecUnderRecovery ==
    /\ Spec
    /\ RelayEventuallyRecovers

NoUnauthorizedClaimSession ==
    unauthorizedAccepted = {}

NoSelfSession ==
    acceptedResponses \cap SelfResponseIds = {}

AtMostOneSessionCreationPerResponse ==
    \A id \in ResponseIds: sessionCreations[id] <= 1

ReplayDoesNotCreateExtraSession ==
    \A id \in replayed: sessionCreations[id] <= 1

AcceptableEventuallyAcceptedUnderRecovery ==
    \A id \in ResponseIds:
        []((id \notin processed /\ IsAcceptable(id)) => <> (id \in acceptedResponses))

====
