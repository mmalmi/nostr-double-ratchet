---- MODULE DirectMessageSubscriptions ----
EXTENDS FiniteSets, Naturals

CONSTANTS
    Authors,
    InitialMessageAuthors,
    InitialSkippedMessageAuthors,
    UpdatedMessageAuthors,
    UpdatedSkippedMessageAuthors,
    BugSessionManagerOwnsDirectSubscriptions,
    BugRuntimeSkipsSync,
    BugRuntimeLeavesStaleSubscriptions,
    BugSessionManagerOmitsSkippedAuthors,
    BugThrottleAddedAuthors

ASSUME Authors # {}
ASSUME InitialMessageAuthors \subseteq Authors
ASSUME InitialSkippedMessageAuthors \subseteq Authors
ASSUME UpdatedMessageAuthors \subseteq Authors
ASSUME UpdatedSkippedMessageAuthors \subseteq Authors
ASSUME BugSessionManagerOwnsDirectSubscriptions \in BOOLEAN
ASSUME BugRuntimeSkipsSync \in BOOLEAN
ASSUME BugRuntimeLeavesStaleSubscriptions \in BOOLEAN
ASSUME BugSessionManagerOmitsSkippedAuthors \in BOOLEAN
ASSUME BugThrottleAddedAuthors \in BOOLEAN

TrackedAuthors(chainAuthors, skippedAuthors) ==
    IF BugSessionManagerOmitsSkippedAuthors
        THEN chainAuthors
        ELSE chainAuthors \cup skippedAuthors

VARIABLES
    managerChainAuthors,
    managerSkippedAuthors,
    managerAuthors,
    runtimeSubscriptionAuthors,
    managerDirectSubscriptionAuthors,
    needsSync,
    throttlePending,
    syncedOnce,
    changeDone,
    relayUp,
    relayBag,
    published,
    delivered

vars ==
    <<managerChainAuthors, managerSkippedAuthors, managerAuthors,
      runtimeSubscriptionAuthors, managerDirectSubscriptionAuthors,
      needsSync, throttlePending, syncedOnce, changeDone,
      relayUp, relayBag, published, delivered>>

Init ==
    /\ managerChainAuthors = InitialMessageAuthors
    /\ managerSkippedAuthors = InitialSkippedMessageAuthors
    /\ managerAuthors = TrackedAuthors(InitialMessageAuthors, InitialSkippedMessageAuthors)
    /\ runtimeSubscriptionAuthors = {}
    /\ managerDirectSubscriptionAuthors =
        IF BugSessionManagerOwnsDirectSubscriptions THEN managerAuthors ELSE {}
    /\ needsSync = (managerAuthors # {})
    /\ throttlePending = FALSE
    /\ syncedOnce = (managerAuthors = {})
    /\ changeDone = FALSE
    /\ relayUp = TRUE
    /\ relayBag = {}
    /\ published = {}
    /\ delivered = {}

SessionStateChanges ==
    /\ ~changeDone
    /\ ~needsSync
    /\ syncedOnce
    /\ LET nextAuthors == TrackedAuthors(UpdatedMessageAuthors, UpdatedSkippedMessageAuthors)
           addedAuthors == nextAuthors \ runtimeSubscriptionAuthors
       IN
       /\ managerChainAuthors' = UpdatedMessageAuthors
       /\ managerSkippedAuthors' = UpdatedSkippedMessageAuthors
       /\ managerAuthors' = nextAuthors
       /\ managerDirectSubscriptionAuthors' =
           IF BugSessionManagerOwnsDirectSubscriptions THEN nextAuthors ELSE {}
       /\ IF addedAuthors # {} /\ ~BugRuntimeSkipsSync /\ ~BugThrottleAddedAuthors
             THEN /\ runtimeSubscriptionAuthors' =
                       IF BugRuntimeLeavesStaleSubscriptions
                           THEN runtimeSubscriptionAuthors \cup nextAuthors
                           ELSE nextAuthors
                  /\ needsSync' = FALSE
                  /\ throttlePending' = FALSE
                  /\ syncedOnce' = TRUE
             ELSE /\ runtimeSubscriptionAuthors' = runtimeSubscriptionAuthors
                  /\ needsSync' = (nextAuthors # runtimeSubscriptionAuthors)
                  /\ throttlePending' = (nextAuthors # runtimeSubscriptionAuthors)
                  /\ syncedOnce' = syncedOnce
    /\ changeDone' = TRUE
    /\ UNCHANGED <<relayUp, relayBag, published, delivered>>

SyncRuntimeSubscription ==
    /\ needsSync
    /\ ~BugRuntimeSkipsSync
    /\ runtimeSubscriptionAuthors' =
        IF BugRuntimeLeavesStaleSubscriptions
            THEN runtimeSubscriptionAuthors \cup managerAuthors
            ELSE managerAuthors
    /\ needsSync' = FALSE
    /\ throttlePending' = FALSE
    /\ syncedOnce' = TRUE
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors, managerAuthors,
        managerDirectSubscriptionAuthors, changeDone,
        relayUp, relayBag, published, delivered
      >>

PublishInbound(a) ==
    /\ a \in managerAuthors
    /\ a \notin published
    /\ relayBag' = relayBag \cup {a}
    /\ published' = published \cup {a}
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors,
        managerAuthors, runtimeSubscriptionAuthors, managerDirectSubscriptionAuthors,
        needsSync, throttlePending, syncedOnce, changeDone, relayUp, delivered
      >>

RelayDeliver(a) ==
    /\ relayUp
    /\ a \in relayBag
    /\ a \in runtimeSubscriptionAuthors
    /\ relayBag' = relayBag \ {a}
    /\ delivered' = delivered \cup {a}
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors,
        managerAuthors, runtimeSubscriptionAuthors, managerDirectSubscriptionAuthors,
        needsSync, throttlePending, syncedOnce, changeDone, relayUp, published
      >>

RelayPartition ==
    /\ relayUp
    /\ relayUp' = FALSE
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors,
        managerAuthors, runtimeSubscriptionAuthors, managerDirectSubscriptionAuthors,
        needsSync, throttlePending, syncedOnce, changeDone, relayBag, published, delivered
      >>

RelayRecover ==
    /\ ~relayUp
    /\ relayUp' = TRUE
    /\ UNCHANGED <<
        managerChainAuthors, managerSkippedAuthors,
        managerAuthors, runtimeSubscriptionAuthors, managerDirectSubscriptionAuthors,
        needsSync, throttlePending, syncedOnce, changeDone, relayBag, published, delivered
      >>

RelayDelay ==
    /\ relayBag # {}
    /\ UNCHANGED vars

Stutter ==
    UNCHANGED vars

RelayEventuallyRecovers ==
    <>[]relayUp

Next ==
    \/ SessionStateChanges
    \/ SyncRuntimeSubscription
    \/ \E a \in Authors: PublishInbound(a)
    \/ \E a \in Authors: RelayDeliver(a)
    \/ RelayPartition
    \/ RelayRecover
    \/ RelayDelay
    \/ Stutter

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(SessionStateChanges)
    /\ WF_vars(SyncRuntimeSubscription)
    /\ \A a \in Authors: WF_vars(PublishInbound(a))
    /\ \A a \in Authors: SF_vars(RelayDeliver(a))

SpecUnderRecovery ==
    /\ Spec
    /\ RelayEventuallyRecovers

SessionManagerDoesNotOwnDirectMessageSubscriptions ==
    managerDirectSubscriptionAuthors = {}

CleanRuntimeSubscriptionMirrorsSessionManager ==
    ~needsSync => runtimeSubscriptionAuthors = managerAuthors

NewAuthorsSubscribedImmediately ==
    syncedOnce => managerAuthors \subseteq runtimeSubscriptionAuthors

SkippedMessageAuthorsAreTracked ==
    managerSkippedAuthors \subseteq managerAuthors

RuntimeSubscriptionAuthorsAreKnown ==
    runtimeSubscriptionAuthors \subseteq Authors

RuntimeSubscriptionEventuallyClean ==
    [](needsSync => <> ~needsSync)

TrackedInboundEventuallyDeliveredUnderRecovery ==
    \A a \in Authors:
        [](a \in published /\ a \in managerAuthors => <> (a \in delivered \/ a \notin managerAuthors))

RemovedAuthorsEventuallyUnsubscribed ==
    \A a \in InitialMessageAuthors \ UpdatedMessageAuthors:
        [](changeDone => <> (a \notin runtimeSubscriptionAuthors))

====
