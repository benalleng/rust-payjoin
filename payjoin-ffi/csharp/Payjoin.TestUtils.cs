// Re-export PayjoinTestUtils types under the Payjoin.TestUtils namespace
// so consumers can use: using Payjoin.TestUtils;

using PayjoinTestUtils;

namespace Payjoin.TestUtils
{
    public static class Methods
    {
        public static string ExampleUrl() => PayjoinTestUtilsMethods.ExampleUrl();
        public static string QueryParams() => PayjoinTestUtilsMethods.QueryParams();
        public static string OriginalPsbt() => PayjoinTestUtilsMethods.OriginalPsbt();
        public static string InvalidPsbt() => PayjoinTestUtilsMethods.InvalidPsbt();
        public static string PayjoinProposal() => PayjoinTestUtilsMethods.PayjoinProposal();
        public static string PayjoinProposalWithSenderInfo() => PayjoinTestUtilsMethods.PayjoinProposalWithSenderInfo();
        public static string ReceiverInputContribution() => PayjoinTestUtilsMethods.ReceiverInputContribution();
        public static BitcoindEnv InitBitcoindSenderReceiver() => PayjoinTestUtilsMethods.InitBitcoindSenderReceiver();
        public static void InitTracing() => PayjoinTestUtilsMethods.InitTracing();
    }
}
