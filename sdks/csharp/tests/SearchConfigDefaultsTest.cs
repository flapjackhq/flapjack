using System;
using Flapjack.Search.Clients;
using Xunit;

namespace Flapjack.Search.Tests;

public class SearchConfigDefaultsTest
{
    [Fact]
    public void GeneratedServiceConfigsUseFiveSecondConnectTimeoutByDefault()
    {
        var configs = new FlapjackConfig[]
        {
            new AbtestingConfig("test-app", "test-api-key"),
            new AbtestingV3Config("test-app", "test-api-key"),
            new AnalyticsConfig("test-app", "test-api-key"),
            new CompositionConfig("test-app", "test-api-key"),
            new InsightsConfig("test-app", "test-api-key"),
            new MonitoringConfig("test-app", "test-api-key"),
            new PersonalizationConfig("test-app", "test-api-key", "us"),
            new QuerySuggestionsConfig("test-app", "test-api-key", "us"),
            new RecommendConfig("test-app", "test-api-key"),
            new SearchConfig("test-app", "test-api-key"),
        };

        foreach (var config in configs)
        {
            Assert.Equal(TimeSpan.FromSeconds(5), config.ConnectTimeout);
        }
    }
}
